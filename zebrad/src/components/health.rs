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
use derive_new::new;

use std::time::Instant;
use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};

use bytes::Bytes;
use chrono::Utc;
use http_body_util::Full;
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::{
    body::Incoming, http::response::Builder as ResponseBuilder, Method, Request, Response,
    StatusCode,
};
use hyper_util::rt::TokioIo;
use tokio::{
    sync::watch,
    task::JoinHandle,
    time::{self, MissedTickBehavior},
};
use tracing::{debug, info, warn};

use zebra_chain::{chain_sync_status::ChainSyncStatus, parameters::Network};
use zebra_network::AddressBookPeers;

// Refresh peers on a short cadence so the cached snapshot tracks live network
// conditions closely without hitting the address book mutex on every request.
const PEER_METRICS_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

const METHOD_NOT_ALLOWED_MSG: &str = "method not allowed";
const NOT_FOUND_MSG: &str = "not found";

/// The maximum number of requests that will be handled in a given time interval before requests are dropped.
const MAX_RECENT_REQUESTS: usize = 10_000;
const RECENT_REQUEST_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct HealthCtx<SyncStatus>
where
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    config: Config,
    network: Network,
    chain_tip_metrics_receiver: watch::Receiver<ChainTipMetrics>,
    sync_status: SyncStatus,
    num_live_peer_receiver: watch::Receiver<usize>,
}

/// Metrics tracking how long it's been since
#[derive(Debug, Clone, PartialEq, Eq, new)]
pub struct ChainTipMetrics {
    /// Last time the chain tip height increased.
    pub last_chain_tip_grow_time: Instant,
    /// Estimated distance between Zebra's chain tip and the network chain tip.
    pub remaining_sync_blocks: Option<i64>,
}

impl ChainTipMetrics {
    /// Creates a new watch channel for reporting [`ChainTipMetrics`].
    pub fn channel() -> (watch::Sender<Self>, watch::Receiver<Self>) {
        watch::channel(Self {
            last_chain_tip_grow_time: Instant::now(),
            remaining_sync_blocks: None,
        })
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
/// # Panics
///
/// - If the configured `listen_addr` cannot be bound.
pub async fn init<AddressBook, SyncStatus>(
    config: Config,
    network: Network,
    chain_tip_metrics_receiver: watch::Receiver<ChainTipMetrics>,
    sync_status: SyncStatus,
    address_book: AddressBook,
) -> (JoinHandle<()>, Option<SocketAddr>)
where
    AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    let Some(listen_addr) = config.listen_addr else {
        return (tokio::spawn(std::future::pending()), None);
    };

    info!("opening health endpoint at {listen_addr}...",);

    let listener = tokio::net::TcpListener::bind(listen_addr)
            .await
            .unwrap_or_else(|e| panic!("Opening health endpoint listener {listen_addr:?} failed: {e:?}. Hint: Check if another zebrad is running, or change the health listen_addr in the config."));

    let local = listener.local_addr().unwrap_or_else(|err| {
        tracing::warn!(?err, "failed to read local addr from TcpListener");
        listen_addr
    });

    info!("opened health endpoint at {}", local);

    let (num_live_peer_sender, num_live_peer_receiver) = watch::channel(0);

    // Seed the watch channel with the first snapshot so early requests see
    // a consistent view even before the refresher loop has ticked.
    if let Some(metrics) = num_live_peers(&address_book).await {
        let _ = num_live_peer_sender.send(metrics);
    }

    // Refresh metrics in the background using a watch channel so request
    // handlers can read the latest snapshot without taking locks.
    let metrics_task = tokio::spawn(peer_metrics_refresh_task(
        address_book.clone(),
        num_live_peer_sender,
    ));

    let shared = Arc::new(HealthCtx {
        config,
        network,
        chain_tip_metrics_receiver,
        sync_status,
        num_live_peer_receiver,
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
}

async fn handle_request<SyncStatus>(
    req: Request<Incoming>,
    ctx: Arc<HealthCtx<SyncStatus>>,
) -> Result<Response<Full<Bytes>>, Infallible>
where
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    // Hyper is already lightweight, but we still fence non-GET methods to keep
    // these endpoints deterministic for probes.
    if req.method() != Method::GET {
        return Ok(simple_response(
            StatusCode::METHOD_NOT_ALLOWED,
            METHOD_NOT_ALLOWED_MSG,
        ));
    }

    let path = req.uri().path();
    let response = match path {
        "/healthy" => healthy(&ctx).await,
        "/ready" => ready(&ctx).await,
        _ => simple_response(StatusCode::NOT_FOUND, NOT_FOUND_MSG),
    };

    Ok(response)
}

// Liveness: ensure we still have the configured minimum of recently live peers,
// matching historical behaviour but fed from the cached snapshot to avoid
// mutex contention.
async fn healthy<SyncStatus>(ctx: &HealthCtx<SyncStatus>) -> Response<Full<Bytes>> {
    if *ctx.num_live_peer_receiver.borrow() >= ctx.config.min_connected_peers {
        simple_response(StatusCode::OK, "ok")
    } else {
        simple_response(StatusCode::SERVICE_UNAVAILABLE, "insufficient peers")
    }
}

// Readiness: combine peer availability, sync progress, estimated lag, and tip
// freshness to avoid the false positives called out in issue #4649 and the
// implementation plan.
async fn ready<SyncStatus>(ctx: &HealthCtx<SyncStatus>) -> Response<Full<Bytes>> {
    if !ctx.config.enforce_on_test_networks && ctx.network.is_a_test_network() {
        return simple_response(StatusCode::OK, "ok");
    }

    if *ctx.num_live_peer_receiver.borrow() < ctx.config.min_connected_peers {
        return simple_response(StatusCode::SERVICE_UNAVAILABLE, "insufficient peers");
    }

    // Keep the historical sync-gate but feed it with the richer readiness
    // checks so we respect the plan's "ensure recent block commits" item.
    if !ctx.sync_status.is_close_to_tip() {
        return simple_response(StatusCode::SERVICE_UNAVAILABLE, "syncing");
    }

    let ChainTipMetrics {
        last_chain_tip_grow_time,
        remaining_sync_blocks,
    } = ctx.chain_tip_metrics_receiver.borrow().clone();

    let Some(remaining_sync_blocks) = remaining_sync_blocks else {
        tracing::warn!("syncer is getting block hashes from peers, but state is empty");
        return simple_response(StatusCode::SERVICE_UNAVAILABLE, "no tip");
    };

    let tip_age = last_chain_tip_grow_time.elapsed();
    if tip_age > ctx.config.ready_max_tip_age {
        return simple_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!("tip_age={}s", tip_age.as_secs()),
        );
    }

    if remaining_sync_blocks <= ctx.config.ready_max_blocks_behind {
        simple_response(StatusCode::OK, "ok")
    } else {
        simple_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!("lag={remaining_sync_blocks} blocks"),
        )
    }
}

// Measure peers on a blocking thread, mirroring the previous synchronous
// implementation but without holding the mutex on the request path.
async fn num_live_peers<A>(address_book: &A) -> Option<usize>
where
    A: AddressBookPeers + Clone + Send + 'static,
{
    let address_book = address_book.clone();
    tokio::task::spawn_blocking(move || address_book.recently_live_peers(Utc::now()).len())
        .await
        .inspect_err(|err| warn!(?err, "failed to refresh peer metrics"))
        .ok()
}

// Periodically update the cached peer metrics for all handlers that hold a
// receiver. If receivers disappear we exit quietly so shutdown can proceed.
async fn peer_metrics_refresh_task<A>(address_book: A, num_live_peer_sender: watch::Sender<usize>)
where
    A: AddressBookPeers + Clone + Send + Sync + 'static,
{
    let mut interval = time::interval(PEER_METRICS_REFRESH_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        // Updates are best-effort: if the snapshot fails or all receivers are
        // dropped we exit quietly, letting the caller terminate the health task.
        if let Some(metrics) = num_live_peers(&address_book).await {
            if let Err(err) = num_live_peer_sender.send(metrics) {
                tracing::warn!(?err, "failed to send to peer metrics channel");
                break;
            }
        }

        interval.tick().await;
    }
}

async fn run_health_server<SyncStatus>(
    listener: tokio::net::TcpListener,
    shared: Arc<HealthCtx<SyncStatus>>,
) where
    SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
{
    let mut num_recent_requests: usize = 0;
    let mut last_request_count_reset_time = Instant::now();

    // Dedicated accept loop to keep request handling small and predictable; we
    // still spawn per-connection tasks but share the context clone.

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                if num_recent_requests < MAX_RECENT_REQUESTS {
                    num_recent_requests += 1;
                } else if last_request_count_reset_time.elapsed() > RECENT_REQUEST_INTERVAL {
                    num_recent_requests = 0;
                    last_request_count_reset_time = Instant::now();
                } else {
                    // Drop the request if there have been too many recent requests
                    continue;
                }

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
