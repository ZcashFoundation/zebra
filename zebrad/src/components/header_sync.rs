//! Headers-first sync: validate and store the header chain before the bodies.
//!
//! [`HeaderSync`] downloads block headers with `getheaders` from legacy peers
//! and `get-headers` from version 2 peers (both are `zn::Request::FindHeaders`,
//! and the peer set routes it to either kind), checks their proof of work with
//! [`zebra_consensus::header::verify_headers`], and stores them with
//! [`zs::Request::CommitHeaders`], which checks their difficulty and time
//! against the previous headers. The block syncers then download bodies only
//! for stored headers.
//!
//! # Below the last checkpoint
//!
//! The chain between two consecutive checkpoints is a span. Spans don't depend
//! on each other, so up to `sync.header_sync_concurrency` of them download at
//! once. A span is requested with the hash at its start and the checkpoint at
//! its end as the stop hash, and continued from its last header until it ends
//! exactly at that checkpoint. Each response's proof of work is checked as it
//! arrives, and the span starts again when a peer answers from its start. Only
//! a complete span is committed, in height order. So a bogus chain from a peer
//! costs at most one span in memory, and never reaches the state.
//!
//! # Above the last checkpoint
//!
//! The header chain is extended one response at a time from a locator over the
//! header chain, until a peer returns no new headers. Then the task polls
//! again on a timer. A response that doesn't connect to the locator is dropped
//! without a penalty: an honest peer on another chain sends those.
//!
//! A response that forks below the header tip, but has less work than the
//! stored headers above the fork point, is continued from its last header over
//! more responses, so a fork deeper than one response can still replace the
//! stored headers once it has more work.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::Utc;
use color_eyre::eyre::{eyre, Report};
use futures::stream::{self, StreamExt};
use tokio::{
    sync::watch,
    time::{sleep, timeout},
};
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{self, Height},
    parameters::{checkpoint::list::CheckpointList, Network},
};
use zebra_consensus::header::verify_headers;
use zebra_network as zn;
use zebra_state as zs;

use crate::BoxError;

#[cfg(test)]
mod tests;

/// The default number of checkpoint spans downloaded at the same time.
pub const DEFAULT_HEADER_SYNC_CONCURRENCY: usize = 8;

/// The most checkpoint spans one round downloads, so the task reads the
/// header tip again regularly.
pub const MAX_SPANS_PER_ROUND: usize = 64;

/// The most requests for one attempt at a checkpoint span, from any peers,
/// on top of the requests the span's length needs.
pub const MAX_SPAN_REQUESTS: usize = 12;

/// The most headers a peer returns in one response, legacy or version 2.
const MAX_HEADERS_PER_RESPONSE: usize = 160;

/// The most requests that continue one fork above the last checkpoint before
/// it is dropped: enough for [`zs::MAX_COMMIT_HEADERS`] headers, with retries.
pub const MAX_FORK_REQUESTS: usize = 32;

/// How many times one round tries each checkpoint span, before it gives up and
/// starts again from the header tip.
pub const MAX_SPAN_ATTEMPTS: usize = 3;

/// How long a peer has to answer a header request.
pub const HEADERS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a state request can take.
pub const HEADER_STATE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the task waits for new headers once a peer has none.
pub const HEADER_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How long the task waits after a failed round.
pub const HEADER_RETRY_DELAY: Duration = Duration::from_secs(5);

/// The shortest time between two info logs of failed rounds. The others are
/// logged at debug.
pub const HEADER_FAILURE_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// The log line when the header chain reaches a peer's tip.
pub const HEADER_SYNC_CAUGHT_UP: &str = "headers-first sync reached a peer's header tip";

/// One checkpoint span: the headers after `start` up to and including `end`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Span {
    start: (Height, block::Hash),
    end: (Height, block::Hash),
}

impl Span {
    /// The number of headers in the span.
    fn len(&self) -> usize {
        // Span ends are above their starts, and heights fit in a usize.
        (self.end.0 .0 - self.start.0 .0) as usize
    }

    /// The height of the span's first header.
    fn first_height(&self) -> Height {
        Height(self.start.0 .0 + 1)
    }
}

/// Downloads, verifies and stores the header chain ahead of the block bodies.
///
/// See the [module docs](self).
pub struct HeaderSync<ZN, ZS> {
    network: Network,
    checkpoints: Arc<CheckpointList>,
    peers: ZN,
    state: ZS,
    concurrency: usize,
    /// Publishes the header chain tip, see [`HeaderSync::with_tip_sender`].
    tip_sender: Option<watch::Sender<Option<Height>>>,
}

impl<ZN, ZS> HeaderSync<ZN, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send + 'static,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send + 'static,
{
    /// Returns a header syncer for `network` that downloads up to `concurrency`
    /// checkpoint spans at a time from `peers`, and stores them in `state`.
    pub fn new(network: &Network, peers: ZN, state: ZS, concurrency: usize) -> Self {
        Self {
            network: network.clone(),
            checkpoints: network.checkpoint_list(),
            peers,
            state,
            concurrency: concurrency.max(1),
            tip_sender: None,
        }
    }

    /// Publishes the header chain tip to `tip_sender` whenever the task reads
    /// or moves it, for the block downloaders that must stay below it.
    pub fn with_tip_sender(mut self, tip_sender: watch::Sender<Option<Height>>) -> Self {
        self.tip_sender = Some(tip_sender);
        self
    }

    /// Records the header chain tip `height` in the metrics and publishes it.
    fn publish_tip(&self, height: Height) {
        metrics::gauge!("sync.headers.tip.height").set(f64::from(height.0));
        if let Some(tip_sender) = self.tip_sender.as_ref() {
            tip_sender.send_if_modified(|published| {
                let changed = *published != Some(height);
                *published = Some(height);
                changed
            });
        }
    }

    /// Runs headers-first sync until Zebra shuts down.
    pub async fn run(mut self) -> Result<(), Report> {
        info!("starting headers-first sync");

        let mut last_failure_log: Option<Instant> = None;
        loop {
            match self.sync_once().await {
                Ok(0) => sleep(HEADER_POLL_INTERVAL).await,
                Ok(_added) => {}
                Err(error) => {
                    // No peer answering is expected, for example before the node has peers,
                    // so this is not an error report.
                    let now = Instant::now();
                    if last_failure_log.is_none_or(|last| {
                        now.saturating_duration_since(last) >= HEADER_FAILURE_LOG_INTERVAL
                    }) {
                        last_failure_log = Some(now);
                        info!("headers-first sync round failed, retrying: {error:#}");
                    } else {
                        debug!("headers-first sync round failed, retrying: {error:#}");
                    }
                    sleep(HEADER_RETRY_DELAY).await;
                }
            }
        }
    }

    /// Runs one round, and returns the number of new headers it stored.
    pub async fn sync_once(&mut self) -> Result<usize, Report> {
        let Some(tip) = self.header_tip().await? else {
            // The syncer commits the genesis block first.
            return Ok(0);
        };

        self.publish_tip(tip.0);

        if tip.0 < self.checkpoints.max_height() {
            self.sync_checkpoint_spans(tip).await
        } else {
            self.extend_header_tip().await
        }
    }

    /// The checkpoint spans above `tip`, at most [`MAX_SPANS_PER_ROUND`].
    fn spans_above(&self, tip: (Height, block::Hash)) -> Vec<Span> {
        let mut start = tip;
        let mut spans = Vec::new();

        for (height, hash) in self.checkpoints.iter_cloned() {
            if height <= start.0 {
                continue;
            }
            if spans.len() >= MAX_SPANS_PER_ROUND {
                break;
            }

            let span = Span {
                start,
                end: (height, hash),
            };
            spans.push(span);
            start = span.end;
        }

        spans
    }

    /// Downloads checkpoint spans above `tip` in parallel, and stores them in
    /// height order. Returns the number of new headers.
    async fn sync_checkpoint_spans(&mut self, tip: (Height, block::Hash)) -> Result<usize, Report> {
        let spans = self.spans_above(tip);
        let peers = self.peers.clone();
        let network = self.network.clone();

        let mut fetched = stream::iter(spans)
            .map(|span| {
                let peers = peers.clone();
                let network = network.clone();
                async move {
                    // Retry only this span, so the spans after it keep downloading.
                    let mut attempt = 1;
                    loop {
                        match fetch_span(peers.clone(), &network, span).await {
                            Ok(headers) => return Ok::<_, Report>((span, headers)),
                            Err(error) if attempt >= MAX_SPAN_ATTEMPTS => return Err(error),
                            Err(error) => {
                                debug!(?error, attempt, "checkpoint span failed, retrying it");
                                attempt += 1;
                            }
                        }
                    }
                }
            })
            .buffered(self.concurrency);

        let mut added = 0;
        while let Some(result) = fetched.next().await {
            let (span, headers) = match result {
                Ok(span) => span,
                Err(error) if added > 0 => {
                    debug!(?error, "checkpoint span failed, starting the next round");
                    break;
                }
                Err(error) => return Err(error),
            };

            for chunk in headers.chunks(zs::MAX_COMMIT_HEADERS) {
                added += self.commit(chunk.to_vec()).await?.added;
            }

            debug!(end = ?span.end, "stored a checkpoint span of headers");
        }

        Ok(added)
    }

    /// Extends the header chain above the last checkpoint with one response
    /// from a peer. Returns the number of new headers.
    async fn extend_header_tip(&mut self) -> Result<usize, Report> {
        let locator = match self.call_state(zs::Request::HeaderLocator).await? {
            zs::Response::HeaderLocator(locator) => locator,
            response => unreachable!("unexpected response to HeaderLocator: {response:?}"),
        };
        if locator.is_empty() {
            return Ok(0);
        }
        let heights: HashMap<block::Hash, Height> = locator
            .iter()
            .map(|(height, hash)| (*hash, *height))
            .collect();

        let headers = request_headers(
            self.peers.clone(),
            locator.iter().map(|(_, hash)| *hash).collect(),
            None,
        )
        .await?;

        let Some(first) = headers.first() else {
            info!(tip = ?locator[0], "{HEADER_SYNC_CAUGHT_UP}");
            return Ok(0);
        };

        let Some(parent_height) = heights.get(&first.previous_block_hash) else {
            debug!("dropping headers that don't connect to the header chain locator");
            return Ok(0);
        };
        let first_height = parent_height
            .next()
            .map_err(|_| eyre!("headers above the maximum height"))?;

        verify_headers(&self.network, first_height, headers.clone(), Utc::now())
            .await
            .map_err(|error| {
                if error.is_misbehavior() {
                    metrics::counter!("sync.headers.invalid.count").increment(1);
                }
                eyre!("a peer sent invalid headers: {error}")
            })?;

        match self.commit(headers.clone()).await {
            Ok(committed) => {
                if committed.added == 0 {
                    info!(tip = ?committed.tip, "{HEADER_SYNC_CAUGHT_UP}");
                }
                Ok(committed.added)
            }
            Err(error) => match error.downcast_ref::<zs::CommitHeadersError>() {
                Some(zs::CommitHeadersError::LessWork { fork_parent }) => {
                    let fork_parent = *fork_parent;
                    self.extend_fork(fork_parent, first_height, headers).await
                }
                Some(commit_error) if !commit_error.is_misbehavior() => {
                    debug!(?commit_error, "headers are not on the best header chain");
                    Ok(0)
                }
                _ => Err(error),
            },
        }
    }

    /// Collects more of a fork whose first headers, `headers` from
    /// `first_height`, have less work than the stored headers above
    /// `fork_parent`, and stores it once it has more work.
    ///
    /// A response holds at most 160 headers, but a fork can replace more stored
    /// headers than that. So the fork is continued
    /// from its last header, and the whole fork is sent to the header writer
    /// after each response, which compares its work with the stored headers
    /// above the fork point. The fork is dropped when a peer has no more of it,
    /// or it reaches [`zs::MAX_COMMIT_HEADERS`] headers. Returns the number of
    /// new headers.
    async fn extend_fork(
        &mut self,
        fork_parent: Height,
        first_height: Height,
        mut headers: Vec<Arc<block::Header>>,
    ) -> Result<usize, Report> {
        for _ in 0..MAX_FORK_REQUESTS {
            if headers.len() >= zs::MAX_COMMIT_HEADERS {
                break;
            }
            let last = headers.last().expect("forks are never empty").hash();

            let mut response = match request_headers(self.peers.clone(), vec![last], None).await {
                Ok(response) => response,
                Err(error) => {
                    debug!(?error, ?fork_parent, "fork header request failed");
                    continue;
                }
            };
            match response.first() {
                // A peer that serves the fork has no more of it.
                None => break,
                Some(first) if first.previous_block_hash != last => continue,
                Some(_) => {}
            }
            response.truncate(zs::MAX_COMMIT_HEADERS - headers.len());

            // The fork fits in a commit batch, so its length fits in a u32.
            let next_height = Height(first_height.0 + headers.len() as u32);
            verify_headers(&self.network, next_height, response.clone(), Utc::now())
                .await
                .map_err(|error| {
                    if error.is_misbehavior() {
                        metrics::counter!("sync.headers.invalid.count").increment(1);
                    }
                    eyre!("a peer sent invalid fork headers: {error}")
                })?;
            headers.extend(response);

            match self.commit(headers.clone()).await {
                Ok(committed) => return Ok(committed.added),
                Err(error) => match error.downcast_ref::<zs::CommitHeadersError>() {
                    Some(zs::CommitHeadersError::LessWork { .. }) => {}
                    Some(commit_error) if !commit_error.is_misbehavior() => {
                        debug!(
                            ?commit_error,
                            "fork headers are not on the best header chain"
                        );
                        return Ok(0);
                    }
                    _ => return Err(error),
                },
            }
        }

        debug!(
            ?fork_parent,
            fork_len = headers.len(),
            "dropping a fork that doesn't have more work than the header chain"
        );
        Ok(0)
    }

    /// Stores verified `headers`.
    async fn commit(
        &mut self,
        headers: Vec<Arc<block::Header>>,
    ) -> Result<zs::HeadersCommitted, Report> {
        let committed = match self.call_state(zs::Request::CommitHeaders(headers)).await? {
            zs::Response::HeadersCommitted(committed) => committed,
            response => unreachable!("unexpected response to CommitHeaders: {response:?}"),
        };

        // A batch has at most `MAX_COMMIT_HEADERS` headers, so the count fits in a u64.
        metrics::counter!("sync.headers.committed.count").increment(committed.added as u64);
        self.publish_tip(committed.tip.0);
        if committed.reorged {
            metrics::counter!("sync.headers.reorg.count").increment(1);
        }

        Ok(committed)
    }

    /// The header chain tip.
    async fn header_tip(&mut self) -> Result<Option<(Height, block::Hash)>, Report> {
        match self.call_state(zs::Request::HeaderTip).await? {
            zs::Response::HeaderTip(tip) => Ok(tip),
            response => unreachable!("unexpected response to HeaderTip: {response:?}"),
        }
    }

    /// Calls the state with a timeout.
    ///
    /// A [`zs::CommitHeadersError`] stays downcastable from the returned report.
    async fn call_state(&mut self, request: zs::Request) -> Result<zs::Response, Report> {
        let state = self.state.clone();
        timeout(HEADER_STATE_TIMEOUT, state.oneshot(request))
            .await
            .map_err(|_| eyre!("the state didn't answer a header request in time"))?
            .map_err(|error| match error.downcast::<zs::CommitHeadersError>() {
                Ok(commit_error) => Report::new(*commit_error),
                Err(error) => eyre!(error),
            })
    }
}

/// Downloads the headers of `span`, continuing from its last header with any
/// peer, until it ends exactly at the span's checkpoint.
///
/// Each response's proof of work is checked before it extends the span. Each
/// request also carries the span's start, so a peer that doesn't have the
/// span's last header answers from the start, and the span starts again from
/// that answer. So one bogus response can't stop honest peers from completing
/// the span.
async fn fetch_span<ZN>(
    peers: ZN,
    network: &Network,
    span: Span,
) -> Result<Vec<Arc<block::Header>>, Report>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send + 'static,
{
    let mut headers: Vec<Arc<block::Header>> = Vec::with_capacity(span.len());
    let mut last = span.start.1;
    // Custom networks can have checkpoints further apart than Mainnet's, so a span can need more
    // than a few responses.
    let max_requests = span
        .len()
        .div_ceil(MAX_HEADERS_PER_RESPONSE)
        .saturating_add(MAX_SPAN_REQUESTS);

    for _ in 0..max_requests {
        let mut known_blocks = vec![last];
        if last != span.start.1 {
            known_blocks.push(span.start.1);
        }

        let response = match request_headers(peers.clone(), known_blocks, Some(span.end.1)).await {
            Ok(response) => response,
            Err(error) => {
                debug!(?error, ?span, "header request failed, asking another peer");
                continue;
            }
        };

        match response.first() {
            None => continue,
            Some(first) if first.previous_block_hash == last => {}
            Some(first) if first.previous_block_hash == span.start.1 => {
                // The peer doesn't have the span's last header, so one of the
                // earlier responses is on another chain.
                debug!(
                    ?span,
                    "a peer answered from the span's start, starting it again"
                );
                headers.clear();
                last = span.start.1;
            }
            Some(_) => {
                // A peer that has neither hash answers from its genesis block,
                // or from its own chain.
                trace!(?span, "dropping headers that don't continue the span");
                continue;
            }
        }

        if headers.len() + response.len() > span.len() {
            debug!(
                ?span,
                "headers ran past the span's checkpoint, starting it again"
            );
            metrics::counter!("sync.headers.invalid.count").increment(1);
            headers.clear();
            last = span.start.1;
            continue;
        }

        // The span is shorter than the gap between two checkpoints, so this fits in a u32.
        let first_height = Height(span.first_height().0 + headers.len() as u32);
        if let Err(error) =
            verify_headers(network, first_height, response.clone(), Utc::now()).await
        {
            if error.is_misbehavior() {
                metrics::counter!("sync.headers.invalid.count").increment(1);
            }
            debug!(
                ?error,
                ?span,
                "dropping invalid headers, asking another peer"
            );
            continue;
        }

        last = response.last().expect("the response is not empty").hash();
        headers.extend(response);

        if headers.len() == span.len() {
            if last == span.end.1 {
                return Ok(headers);
            }

            debug!(
                ?span,
                "headers don't end at the span's checkpoint, starting it again"
            );
            metrics::counter!("sync.headers.invalid.count").increment(1);
            headers.clear();
            last = span.start.1;
        }
    }

    Err(eyre!(
        "no peer completed the checkpoint span {span:?} in {max_requests} requests"
    ))
}

/// Requests the headers after the first of `known_blocks` that a peer has, up
/// to and including `stop`.
async fn request_headers<ZN>(
    peers: ZN,
    known_blocks: Vec<block::Hash>,
    stop: Option<block::Hash>,
) -> Result<Vec<Arc<block::Header>>, Report>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send + 'static,
{
    let response = timeout(
        HEADERS_RESPONSE_TIMEOUT,
        peers.oneshot(zn::Request::FindHeaders { known_blocks, stop }),
    )
    .await
    .map_err(|_| eyre!("no header response in time"))?
    .map_err(|error| eyre!(error))?;

    match response {
        zn::Response::BlockHeaders(headers) => {
            Ok(headers.into_iter().map(|counted| counted.header).collect())
        }
        response => Err(eyre!("unexpected response to FindHeaders: {response:?}")),
    }
}
