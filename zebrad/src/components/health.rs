//! HTTP health and readiness endpoints for `zebrad`.
//!
//! Overview
//!
//! - This module exposes two small HTTP/1.1 endpoints for basic liveness/readiness checks,
//!   suitable for Kubernetes probes and load balancers.
//! - Endpoints are opt-in, disabled by default. Enable by setting a `listen_addr` in the
//!   `health` config section.
//! - Plain-text responses and small responses keep the checks fast and safe.
//!
//! Endpoints
//!
//! - `GET /healthy` — returns `200 OK` if the process is up and the node has at least
//!   `min_connected_peers` recently live peers (default: 1). Otherwise `503 Service Unavailable`.
//! - `GET /ready` — returns `200 OK` if the node is near the chain tip, the estimated block lag is
//!   within `ready_max_blocks_behind`, and the latest committed block is recent. On regtest/testnet,
//!   readiness returns `200` unless `enforce_on_test_networks` is set.
//!
//! Security
//!
//! - Endpoints are unauthenticated by design. Bind to internal network interfaces,
//!   and restrict exposure using network policy, firewall rules, and service configuration.
//! - The server does not access or return private data. It only summarises coarse node state.
//!
//! Configuration and examples
//!
//! - See the Zebra Book for configuration details and Kubernetes probe examples:
//!   <https://zebra.zfnd.org/user/health.html>

mod config;
#[cfg(test)]
mod tests;

pub use config::Config;

use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};

use bytes::Bytes;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use http_body_util::Full;
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::{
    body::Incoming, http::response::Builder as ResponseBuilder, Method, Request, Response,
    StatusCode,
};
use hyper_util::rt::TokioIo;
use tokio::{
    sync::{watch, Semaphore},
    task::JoinHandle,
    time::{self, MissedTickBehavior},
};
use tracing::{debug, info, warn};

use zebra_chain::{chain_sync_status::ChainSyncStatus, chain_tip::ChainTip, parameters::Network};
use zebra_network::AddressBookPeers;

// Refresh peers on a short cadence so the cached snapshot tracks live network
// conditions closely without hitting the address book mutex on every request.
const PEER_METRICS_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
// Mark a snapshot as stale when it is several intervals old – this balances the
// cost of recomputing the set with the need to avoid serving very out-of-date
// liveness data (implementation-plan item #1).
const PEER_METRICS_STALE_MULTIPLIER: i32 = 3;
const TOO_MANY_REQUESTS: &str = "too many requests";
const METHOD_NOT_ALLOWED: &str = "method not allowed";
const NOT_FOUND: &str = "not found";

/// Cached view of recently live peers shared via `watch`.
#[derive(Clone, Debug)]
struct PeerMetrics {
    last_updated: DateTime<Utc>,
    live_peers: usize,
}

impl Default for PeerMetrics {
    fn default() -> Self {
        Self {
            last_updated: Utc::now(),
            live_peers: 0,
        }
    }
}

/// Lightweight, process-wide rate limiter for the health endpoints.
///
/// Zebra typically applies rate limits at the network edge, but the health
/// handlers need a final guard to avoid becoming accidental hot paths. When the
/// configured interval is zero we disable throttling entirely, matching
/// existing component behaviour where `Duration::ZERO` means "off".
#[derive(Clone)]
struct RateLimiter {
    permits: Option<Arc<Semaphore>>,
    interval: Duration,
}

impl RateLimiter {
    fn new(interval: Duration) -> Self {
        let permits = if interval.is_zero() {
            None
        } else {
            Some(Arc::new(Semaphore::new(1)))
        };

        Self { permits, interval }
    }

    fn allow(&self) -> bool {
        let Some(permits) = &self.permits else {
            return true;
        };

        match permits.clone().try_acquire_owned() {
            Ok(permit) => {
                let interval = self.interval;
                tokio::spawn(async move {
                    // Release the single permit after the configured cool-down so
                    // the next request can proceed.
                    time::sleep(interval).await;
                    drop(permit);
                });
                true
            }
            Err(_) => false,
        }
    }
}

#[derive(Clone)]
struct HealthCtx<Tip, SyncStatus>
where
    Tip: ChainTip + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    config: Config,
    network: Network,
    latest_chain_tip: Tip,
    sync_status: SyncStatus,
    peer_metrics_rx: watch::Receiver<PeerMetrics>,
    rate_limiter: RateLimiter,
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

        let listener = tokio::net::TcpListener::bind(listen_addr)
            .await
            .unwrap_or_else(|e| panic!("Opening health endpoint listener {listen_addr:?} failed: {e:?}. Hint: Check if another zebrad is running, or change the health listen_addr in the config."));

        let local = listener.local_addr().unwrap_or(listen_addr);
        info!("Opened health endpoint at {}", local);

        let (peer_metrics_tx, peer_metrics_rx) = watch::channel(PeerMetrics::default());

        // Seed the watch channel with the first snapshot so early requests see
        // a consistent view even before the refresher loop has ticked.
        if let Some(metrics) = measure_peer_metrics(&address_book).await {
            let _ = peer_metrics_tx.send(metrics);
        }

        // Refresh metrics in the background using a watch channel so request
        // handlers can read the latest snapshot without taking locks.
        let metrics_task = tokio::spawn(peer_metrics_refresh_task(
            address_book.clone(),
            peer_metrics_tx,
        ));

        let shared = Arc::new(HealthCtx {
            rate_limiter: RateLimiter::new(config.min_request_interval),
            config,
            network,
            latest_chain_tip,
            sync_status,
            peer_metrics_rx,
        });

        let server_task = tokio::spawn(run_health_server(listener, shared));

        // Keep both async tasks tied to a single JoinHandle so shutdown and
        // abort semantics mirror other components.
        let task = tokio::spawn(async move {
            tokio::select! {
                _ = metrics_task => {},
                _ = server_task => {},
            }
        });

        (task, Some(local))
    } else {
        (tokio::spawn(std::future::pending()), None)
    }
}

async fn handle_request<Tip, SyncStatus>(
    req: Request<Incoming>,
    ctx: Arc<HealthCtx<Tip, SyncStatus>>,
) -> Result<Response<Full<Bytes>>, Infallible>
where
    Tip: ChainTip + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    // Hyper is already lightweight, but we still fence non-GET methods to keep
    // these endpoints deterministic for probes.
    if req.method() != Method::GET {
        return Ok(simple_response(
            StatusCode::METHOD_NOT_ALLOWED,
            METHOD_NOT_ALLOWED,
        ));
    }

    if !ctx.rate_limiter.allow() {
        return Ok(simple_response(
            StatusCode::TOO_MANY_REQUESTS,
            TOO_MANY_REQUESTS,
        ));
    }

    let path = req.uri().path();
    let response = match path {
        "/healthy" => healthy(&ctx).await,
        "/ready" => ready(&ctx).await,
        _ => simple_response(StatusCode::NOT_FOUND, NOT_FOUND),
    };

    Ok(response)
}

// Liveness: ensure we still have the configured minimum of recently live peers,
// matching historical behaviour but fed from the cached snapshot to avoid
// mutex contention.
async fn healthy<Tip, SyncStatus>(ctx: &HealthCtx<Tip, SyncStatus>) -> Response<Full<Bytes>>
where
    Tip: ChainTip + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    let metrics = (*ctx.peer_metrics_rx.borrow()).clone();

    // Refuse to serve responses when the peer view is older than our refresh
    // cadence; this prevents the snapshot from masking outages.
    if peer_metrics_are_stale(&metrics) {
        return simple_response(StatusCode::SERVICE_UNAVAILABLE, "stale peer metrics");
    }

    if metrics.live_peers >= ctx.config.min_connected_peers {
        simple_response(StatusCode::OK, "ok")
    } else {
        simple_response(StatusCode::SERVICE_UNAVAILABLE, "no peers")
    }
}

// Readiness: combine peer availability, sync progress, estimated lag, and tip
// freshness to avoid the false positives called out in issue #4649 and the
// implementation plan.
async fn ready<Tip, SyncStatus>(ctx: &HealthCtx<Tip, SyncStatus>) -> Response<Full<Bytes>>
where
    Tip: ChainTip + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    if !ctx.config.enforce_on_test_networks && ctx.network.is_a_test_network() {
        return simple_response(StatusCode::OK, "ok");
    }

    let metrics = (*ctx.peer_metrics_rx.borrow()).clone();

    if peer_metrics_are_stale(&metrics) {
        return simple_response(StatusCode::SERVICE_UNAVAILABLE, "stale peer metrics");
    }

    if metrics.live_peers < ctx.config.min_connected_peers {
        return simple_response(StatusCode::SERVICE_UNAVAILABLE, "no peers");
    }

    // Keep the historical sync-gate but feed it with the richer readiness
    // checks so we respect the plan's "ensure recent block commits" item.
    if !ctx.sync_status.is_close_to_tip() {
        return simple_response(StatusCode::SERVICE_UNAVAILABLE, "syncing");
    }

    let tip_time = match ctx.latest_chain_tip.best_tip_block_time() {
        Some(time) => time,
        None => return simple_response(StatusCode::SERVICE_UNAVAILABLE, "no tip"),
    };

    let mut tip_age = Utc::now().signed_duration_since(tip_time);
    if tip_age < ChronoDuration::zero() {
        tip_age = ChronoDuration::zero();
    }

    let Ok(max_age) = ChronoDuration::from_std(ctx.config.ready_max_tip_age) else {
        return simple_response(StatusCode::SERVICE_UNAVAILABLE, "invalid tip age limit");
    };

    if tip_age > max_age {
        return simple_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!("tip_age={}s", tip_age.num_seconds()),
        );
    }

    match ctx
        .latest_chain_tip
        .estimate_distance_to_network_chain_tip(&ctx.network)
    {
        Some((distance, _local_tip)) => {
            let behind = std::cmp::max(0, distance);
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

fn peer_metrics_are_stale(metrics: &PeerMetrics) -> bool {
    let age = Utc::now().signed_duration_since(metrics.last_updated);
    if age <= ChronoDuration::zero() {
        return false;
    }

    match ChronoDuration::from_std(PEER_METRICS_REFRESH_INTERVAL) {
        Ok(refresh) => age > refresh * PEER_METRICS_STALE_MULTIPLIER,
        Err(_) => false,
    }
}

// Measure peers on a blocking thread, mirroring the previous synchronous
// implementation but without holding the mutex on the request path.
async fn measure_peer_metrics<A>(address_book: &A) -> Option<PeerMetrics>
where
    A: AddressBookPeers + Clone + Send + Sync + 'static,
{
    let address_book = address_book.clone();
    match tokio::task::spawn_blocking(move || {
        let now = Utc::now();
        let live_peers = address_book.recently_live_peers(now).len();
        PeerMetrics {
            last_updated: now,
            live_peers,
        }
    })
    .await
    {
        Ok(metrics) => Some(metrics),
        Err(err) => {
            warn!(?err, "failed to refresh peer metrics");
            None
        }
    }
}

// Periodically update the cached peer metrics for all handlers that hold a
// receiver. If receivers disappear we exit quietly so shutdown can proceed.
async fn peer_metrics_refresh_task<A>(address_book: A, peer_metrics_tx: watch::Sender<PeerMetrics>)
where
    A: AddressBookPeers + Clone + Send + Sync + 'static,
{
    let mut interval = time::interval(PEER_METRICS_REFRESH_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        interval.tick().await;

        // Updates are best-effort: if the snapshot fails or all receivers are
        // dropped we exit quietly, letting the caller terminate the health task.
        if let Some(metrics) = measure_peer_metrics(&address_book).await {
            if peer_metrics_tx.send(metrics).is_err() {
                break;
            }
        }
    }
}

async fn run_health_server<Tip, SyncStatus>(
    listener: tokio::net::TcpListener,
    shared: Arc<HealthCtx<Tip, SyncStatus>>,
) where
    Tip: ChainTip + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    // Dedicated accept loop to keep request handling small and predictable; we
    // still spawn per-connection tasks but share the context clone.
    loop {
        match listener.accept().await {
            Ok((stream, _peer)) => {
                let io = TokioIo::new(stream);
                let svc_ctx = shared.clone();
                let service =
                    hyper::service::service_fn(move |req| handle_request(req, svc_ctx.clone()));

                tokio::spawn(async move {
                    if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                        debug!(?err, "health server connection closed with error");
                    }
                });
            }
            Err(err) => {
                warn!(?err, "health server accept failed");
            }
        }
    }
}

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
