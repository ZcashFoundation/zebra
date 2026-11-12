//! Fetches note commitment tree roots from peers for fast checkpoint sync.

use std::{
    ops::RangeInclusive,
    time::{Duration, Instant},
};

use tokio::time::{sleep, timeout};
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{Height, HeightDiff},
    parallel::tree::MAX_TREE_ROOTS_PER_REQUEST,
    parameters::{checkpoint::list::CheckpointList, Network},
};
use zebra_network as zn;
use zebra_state as zs;

use crate::BoxError;

#[cfg(test)]
mod tests;

/// How long to wait for a peer's tree roots, including peer set readiness.
const TREE_ROOTS_TIMEOUT: Duration = Duration::from_secs(20);

/// How long to wait before asking the state again when it doesn't need any roots.
const IDLE_DELAY: Duration = Duration::from_secs(5);

/// How long to wait before retrying after a peer refused or returned no roots.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// How long to wait before retrying after a failed request, for example when no ready peer
/// serves tree roots.
const ERROR_RETRY_DELAY: Duration = Duration::from_secs(5);

/// How often to log failed requests at info level.
const FAILURE_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// Fetches the note commitment tree roots the state wants from peers, and supplies them to the
/// state, forever.
///
/// Returns an error if the state fails.
pub async fn fetch_tree_roots<ZN, ZS>(
    network: Network,
    peers: ZN,
    state: ZS,
) -> Result<(), BoxError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Clone + Send + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Clone + Send + 'static,
    ZS::Future: Send,
{
    let checkpoints = network.checkpoint_list();
    let mut started = false;
    let mut last_failure_log: Option<Instant> = None;

    loop {
        let zs::Response::WantedTreeRoots(wanted) =
            state.clone().oneshot(zs::Request::WantedTreeRoots).await?
        else {
            unreachable!("zebra-state responds to `WantedTreeRoots` with `WantedTreeRoots`");
        };
        let Some((start_height, request)) = wanted
            .and_then(|wanted| Some((*wanted.start(), tree_roots_request(&checkpoints, wanted)?)))
        else {
            sleep(IDLE_DELAY).await;
            continue;
        };

        if !started {
            info!(
                ?start_height,
                "fast tree sync: fetching note commitment tree roots from peers"
            );
            started = true;
        }

        let (delay, failure) =
            match timeout(TREE_ROOTS_TIMEOUT, peers.clone().oneshot(request)).await {
                Ok(Ok(zn::Response::TreeRoots(Some(roots)))) if !roots.is_empty() => {
                    state
                        .clone()
                        .oneshot(zs::Request::SupplyTreeRoots {
                            start_height,
                            roots,
                        })
                        .await?;
                    continue;
                }
                Ok(Ok(response)) => (RETRY_DELAY, format!("peer returned {response}")),
                Ok(Err(error)) => (ERROR_RETRY_DELAY, error.to_string()),
                Err(_) => (ERROR_RETRY_DELAY, "timed out".to_string()),
            };

        // Sync waits for these roots once it skips tree updates, so report failures that last.
        if last_failure_log.is_none_or(|logged| logged.elapsed() >= FAILURE_LOG_INTERVAL) {
            info!(?start_height, %failure, "fast tree sync: tree roots request failed");
            last_failure_log = Some(Instant::now());
        } else {
            debug!(?start_height, %failure, "fast tree sync: tree roots request failed");
        }
        sleep(delay).await;
    }
}

/// Returns a request for the roots from the start of `wanted`, ending at a checkpoint.
///
/// Prefers the highest checkpoint within `wanted` and the request limit, and falls back to the
/// lowest checkpoint within the limit, which can be after the end of `wanted`.
fn tree_roots_request(
    checkpoints: &CheckpointList,
    wanted: RangeInclusive<Height>,
) -> Option<zn::Request> {
    let start_height = *wanted.start();
    let limit = (start_height + HeightDiff::from(MAX_TREE_ROOTS_PER_REQUEST - 1))?;
    let final_height = checkpoints
        .max_height_in_range(start_height..=limit.min(*wanted.end()))
        .or_else(|| checkpoints.min_height_in_range(start_height..=limit))?;

    Some(zn::Request::TreeRoots {
        start_height,
        final_hash: checkpoints.hash(final_height)?,
        count: u32::try_from(final_height - start_height + 1)
            .expect("the final height is within the request limit of the start height"),
    })
}
