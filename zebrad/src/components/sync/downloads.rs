//! A download stream for Zebra's block syncer.

use std::{
    collections::HashMap,
    convert,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{
    future::{FutureExt, TryFutureExt},
    ready,
    stream::{FuturesUnordered, Stream},
};
use pin_project::pin_project;
use thiserror::Error;
use tokio::{
    sync::{oneshot, watch},
    task::JoinHandle,
    time::timeout,
};
use tower::{hedge, Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::{
    block::{self, Height, HeightDiff},
    chain_tip::ChainTip,
};
use zebra_network::{self as zn, PeerSocketAddr};
use zebra_state as zs;

use crate::components::sync::{
    FINAL_CHECKPOINT_BLOCK_VERIFY_TIMEOUT, FINAL_CHECKPOINT_BLOCK_VERIFY_TIMEOUT_LIMIT,
};

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// A multiplier used to calculate the extra number of blocks we allow in the
/// verifier, state, and block commit pipelines, on top of the lookahead limit.
///
/// The extra number of blocks is calculated using
/// `lookahead_limit * VERIFICATION_PIPELINE_SCALING_MULTIPLIER`.
///
/// This allows the verifier and state queues, and the block commit channel,
/// to hold a few extra tips responses worth of blocks,
/// even if the syncer queue is full. Any unused capacity is shared between both queues.
///
/// If this capacity is exceeded, the downloader will tell the syncer to pause new downloads.
///
/// Since the syncer queue is limited to the `lookahead_limit`,
/// the rest of the capacity is reserved for the other queues.
/// There is no reserved capacity for the syncer queue:
/// if the other queues stay full, the syncer will eventually time out and reset.
pub const VERIFICATION_PIPELINE_SCALING_MULTIPLIER: usize = 2;

/// The maximum height difference between Zebra's state tip and a downloaded block.
/// Blocks higher than this will get dropped and return an error.
pub const VERIFICATION_PIPELINE_DROP_LIMIT: HeightDiff = 50_000;

#[derive(Copy, Clone, Debug)]
pub(super) struct AlwaysHedge;

impl<Request: Clone> hedge::Policy<Request> for AlwaysHedge {
    fn can_retry(&self, _req: &Request) -> bool {
        true
    }
    fn clone_request(&self, req: &Request) -> Option<Request> {
        Some(req.clone())
    }
}

/// Errors that can occur while downloading and verifying a block.
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum BlockDownloadVerifyError {
    #[error("permanent readiness error from the network service: {error:?}")]
    NetworkServiceError {
        #[source]
        error: BoxError,
    },

    #[error("permanent readiness error from the verifier service: {error:?}")]
    VerifierServiceError {
        #[source]
        error: BoxError,
    },

    #[error("duplicate block hash queued for download: {hash:?}")]
    DuplicateBlockQueuedForDownload { hash: block::Hash },

    #[error("error downloading block: {error:?} {hash:?}")]
    DownloadFailed {
        #[source]
        error: BoxError,
        hash: block::Hash,
    },

    /// A downloaded block was a long way ahead of the state chain tip.
    /// This error should be very rare during normal operation.
    ///
    /// We need to reset the syncer on this error, to allow the verifier and state to catch up,
    /// or prevent it following a bad chain.
    ///
    /// If we don't reset the syncer on this error, it will continue downloading blocks from a bad
    /// chain, or blocks far ahead of the current state tip.
    #[error("downloaded block was too far ahead of the chain tip: {height:?} {hash:?}")]
    AboveLookaheadHeightLimit {
        height: block::Height,
        hash: block::Hash,
    },

    #[error("downloaded block was too far behind the chain tip: {height:?} {hash:?}")]
    BehindTipHeightLimit {
        height: block::Height,
        hash: block::Hash,
    },

    #[error("downloaded block had an invalid height: {hash:?}")]
    InvalidHeight { hash: block::Hash },

    #[error("block failed consensus validation: {error:?} {height:?} {hash:?}")]
    Invalid {
        #[source]
        error: zebra_consensus::router::RouterError,
        height: block::Height,
        hash: block::Hash,
        advertiser_addr: Option<PeerSocketAddr>,
    },

    #[error("block validation request failed: {error:?} {height:?} {hash:?}")]
    ValidationRequestError {
        #[source]
        error: BoxError,
        height: block::Height,
        hash: block::Hash,
    },

    #[error("block download & verification was cancelled during download: {hash:?}")]
    CancelledDuringDownload { hash: block::Hash },

    #[error(
        "block download & verification was cancelled while waiting for the verifier service: \
         to become ready: {height:?} {hash:?}"
    )]
    CancelledAwaitingVerifierReadiness {
        height: block::Height,
        hash: block::Hash,
    },

    #[error(
        "block download & verification was cancelled during verification: {height:?} {hash:?}"
    )]
    CancelledDuringVerification {
        height: block::Height,
        hash: block::Hash,
    },

    #[error(
        "timeout during service readiness, download, verification, or internal downloader operation"
    )]
    Timeout,
}

impl From<tokio::time::error::Elapsed> for BlockDownloadVerifyError {
    fn from(_value: tokio::time::error::Elapsed) -> Self {
        BlockDownloadVerifyError::Timeout
    }
}

/// Represents a [`Stream`] of download and verification tasks during chain sync.
#[pin_project]
#[derive(Debug)]
pub struct Downloads<ZN, ZV, ZSTip>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Sync + 'static,
    ZN::Future: Send,
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZV::Future: Send,
    ZSTip: ChainTip + Clone + Send + 'static,
{
    // Services
    //
    /// A service that forwards requests to connected peers, and returns their
    /// responses.
    network: ZN,

    /// A service that verifies downloaded blocks.
    verifier: ZV,

    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: ZSTip,

    // Configuration
    //
    /// The configured lookahead limit, after applying the minimum limit.
    lookahead_limit: usize,

    /// The largest block height for the checkpoint verifier, based on the current config.
    max_checkpoint_height: Height,

    // Shared syncer state
    //
    /// Sender that is set to `true` when the downloader is past the lookahead limit.
    /// This is based on the downloaded block height and the state tip height.
    past_lookahead_limit_sender: Arc<std::sync::Mutex<watch::Sender<bool>>>,

    /// Receiver for `past_lookahead_limit_sender`, which is used to avoid accessing the mutex.
    past_lookahead_limit_receiver: zs::WatchReceiver<bool>,

    // Internal downloads state
    //
    /// A list of pending block download and verify tasks.
    #[pin]
    pending: FuturesUnordered<
        JoinHandle<Result<(Height, block::Hash), (BlockDownloadVerifyError, block::Hash)>>,
    >,

    /// A list of channels that can be used to cancel pending block download and
    /// verify tasks.
    cancel_handles: HashMap<block::Hash, oneshot::Sender<()>>,
}

impl<ZN, ZV, ZSTip> Stream for Downloads<ZN, ZV, ZSTip>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Sync + 'static,
    ZN::Future: Send,
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZV::Future: Send,
    ZSTip: ChainTip + Clone + Send + 'static,
{
    type Item = Result<(Height, block::Hash), BlockDownloadVerifyError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let this = self.project();
        // CORRECTNESS
        //
        // The current task must be scheduled for wakeup every time we return
        // `Poll::Pending`.
        //
        // If no download and verify tasks have exited since the last poll, this
        // task is scheduled for wakeup when the next task becomes ready.
        //
        // TODO: this would be cleaner with poll_map (#2693)
        if let Some(join_result) = ready!(this.pending.poll_next(cx)) {
            match join_result.expect("block download and verify tasks must not panic") {
                Ok((height, hash)) => {
                    this.cancel_handles.remove(&hash);

                    Poll::Ready(Some(Ok((height, hash))))
                }
                Err((e, hash)) => {
                    this.cancel_handles.remove(&hash);
                    Poll::Ready(Some(Err(e)))
                }
            }
        } else {
            Poll::Ready(None)
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.pending.size_hint()
    }
}

impl<ZN, ZV, ZSTip> Downloads<ZN, ZV, ZSTip>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Sync + 'static,
    ZN::Future: Send,
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZV::Future: Send,
    ZSTip: ChainTip + Clone + Send + 'static,
{
    /// Initialize a new download stream with the provided `network` and
    /// `verifier` services.
    ///
    /// Uses the `latest_chain_tip` and `lookahead_limit` to drop blocks
    /// that are too far ahead of the current state tip.
    /// Uses `max_checkpoint_height` to work around a known block timeout (#5125).
    ///
    /// The [`Downloads`] stream is agnostic to the network policy, so retry and
    /// timeout limits should be applied to the `network` service passed into
    /// this constructor.
    pub fn new(
        network: ZN,
        verifier: ZV,
        latest_chain_tip: ZSTip,
        past_lookahead_limit_sender: watch::Sender<bool>,
        lookahead_limit: usize,
        max_checkpoint_height: Height,
    ) -> Self {
        let past_lookahead_limit_receiver =
            zs::WatchReceiver::new(past_lookahead_limit_sender.subscribe());

        Self {
            network,
            verifier,
            latest_chain_tip,
            lookahead_limit,
            max_checkpoint_height,
            past_lookahead_limit_sender: Arc::new(std::sync::Mutex::new(
                past_lookahead_limit_sender,
            )),
            past_lookahead_limit_receiver,
            pending: FuturesUnordered::new(),
            cancel_handles: HashMap::new(),
        }
    }

    /// Queue a block for download and verification.
    ///
    /// This method waits for the network to become ready, and returns an error
    /// only if the network service fails. It returns immediately after queuing
    /// the request.
    #[instrument(level = "debug", skip(self), fields(%hash))]
    pub async fn download_and_verify(
        &mut self,
        hash: block::Hash,
    ) -> Result<(), BlockDownloadVerifyError> {
        if self.cancel_handles.contains_key(&hash) {
            metrics::counter!("sync.already.queued.dropped.block.hash.count").increment(1);
            return Err(BlockDownloadVerifyError::DuplicateBlockQueuedForDownload { hash });
        }

        // We construct the block requests sequentially, waiting for the peer
        // set to be ready to process each request. This ensures that we start
        // block downloads in the order we want them (though they may resolve
        // out of order), and it means that we respect backpressure. Otherwise,
        // if we waited for readiness and did the service call in the spawned
        // tasks, all of the spawned tasks would race each other waiting for the
        // network to become ready.
        let block_req = self
            .network
            .ready()
            .await
            .map_err(|error| BlockDownloadVerifyError::NetworkServiceError { error })?
            .call(zn::Request::BlocksByHash(std::iter::once(hash).collect()));

        // This oneshot is used to signal cancellation to the download task.
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();

        let mut verifier = self.verifier.clone();
        let latest_chain_tip = self.latest_chain_tip.clone();

        let lookahead_limit = self.lookahead_limit;
        let max_checkpoint_height = self.max_checkpoint_height;

        let past_lookahead_limit_sender = self.past_lookahead_limit_sender.clone();
        let past_lookahead_limit_receiver = self.past_lookahead_limit_receiver.clone();

        let task = tokio::spawn(
            async move {
                // Download the block.
                // Prefer the cancel handle if both are ready.
                let download_start = std::time::Instant::now();
                let rsp = tokio::select! {
                    biased;
                    _ = &mut cancel_rx => {
                        trace!("task cancelled prior to download completion");
                        metrics::counter!("sync.cancelled.download.count").increment(1);
                        metrics::histogram!("sync.block.download.duration_seconds", "result" => "cancelled")
                            .record(download_start.elapsed().as_secs_f64());
                        return Err(BlockDownloadVerifyError::CancelledDuringDownload { hash })
                    }
                    rsp = block_req => rsp.map_err(|error| BlockDownloadVerifyError::DownloadFailed { error, hash})?,
                };

                let (block, advertiser_addr) = if let zn::Response::Blocks(blocks) = rsp {
                    assert_eq!(
                        blocks.len(),
                        1,
                        "wrong number of blocks in response to a single hash"
                    );

                    blocks
                        .first()
                        .expect("just checked length")
                        .available()
                        .expect("unexpected missing block status: single block failures should be errors")
                } else {
                    unreachable!("wrong response to block request");
                };
                metrics::counter!("sync.downloaded.block.count").increment(1);
                metrics::histogram!("sync.block.download.duration_seconds", "result" => "success")
                    .record(download_start.elapsed().as_secs_f64());

                // Security & Performance: reject blocks that are too far ahead of our tip.
                // Avoids denial of service attacks, and reduces wasted work on high blocks
                // that will timeout before being verified.
                let tip_height = latest_chain_tip.best_tip_height();

                let (lookahead_drop_height, lookahead_pause_height, lookahead_reset_height) = if let Some(tip_height) = tip_height {
                    // Scale the height limit with the lookahead limit,
                    // so users with low capacity or under DoS can reduce them both.
                    let lookahead_pause = HeightDiff::try_from(
                        lookahead_limit + lookahead_limit * VERIFICATION_PIPELINE_SCALING_MULTIPLIER,
                    )
                        .expect("fits in HeightDiff");


                    ((tip_height + VERIFICATION_PIPELINE_DROP_LIMIT).expect("tip is much lower than Height::MAX"),
                     (tip_height + lookahead_pause).expect("tip is much lower than Height::MAX"),
                     (tip_height + lookahead_pause/2).expect("tip is much lower than Height::MAX"))
                } else {
                    let genesis_drop = VERIFICATION_PIPELINE_DROP_LIMIT.try_into().expect("fits in u32");
                    let genesis_lookahead =
                        u32::try_from(lookahead_limit - 1).expect("fits in u32");

                    (block::Height(genesis_drop),
                     block::Height(genesis_lookahead),
                     block::Height(genesis_lookahead/2))
                };

                // Get the finalized tip height, assuming we're using the non-finalized state.
                //
                // It doesn't matter if we're a few blocks off here, because blocks this low
                // are part of a fork with much less work. So they would be rejected anyway.
                //
                // And if we're still checkpointing, the checkpointer will reject blocks behind
                // the finalized tip anyway.
                //
                // TODO: get the actual finalized tip height
                let min_accepted_height = tip_height
                    .map(|tip_height| {
                        block::Height(tip_height.0.saturating_sub(zs::MAX_BLOCK_REORG_HEIGHT))
                    })
                    .unwrap_or(block::Height(0));

                let block_height = if let Some(block_height) = block.coinbase_height() {
                    block_height
                } else {
                    debug!(
                        ?hash,
                        "synced block with no height: dropped downloaded block"
                    );
                    metrics::counter!("sync.no.height.dropped.block.count").increment(1);

                    return Err(BlockDownloadVerifyError::InvalidHeight { hash });
                };

                if block_height > lookahead_drop_height {
                    Err(BlockDownloadVerifyError::AboveLookaheadHeightLimit { height: block_height, hash })?;
                } else if block_height > lookahead_pause_height {
                    // This log can be very verbose, usually hundreds of blocks are dropped.
                    // So we only log at info level for the first above-height block.
                    if !past_lookahead_limit_receiver.cloned_watch_data() {
                        info!(
                            ?hash,
                            ?block_height,
                            ?tip_height,
                            ?lookahead_pause_height,
                            ?lookahead_reset_height,
                            lookahead_limit = ?lookahead_limit,
                            "synced block height too far ahead of the tip: \
                             waiting for downloaded blocks to commit to the state",
                        );

                        // Set the watched value to true, since we're over the limit.
                        //
                        // It is ok to block here, because we're going to pause new downloads anyway.
                        // But if Zebra is shutting down, ignore the send error.
                        let _ = past_lookahead_limit_sender.lock().expect("thread panicked while holding the past_lookahead_limit_sender mutex guard").send(true);
                    } else {
                        debug!(
                            ?hash,
                            ?block_height,
                            ?tip_height,
                            ?lookahead_pause_height,
                            ?lookahead_reset_height,
                            lookahead_limit = ?lookahead_limit,
                            "synced block height too far ahead of the tip: \
                             waiting for downloaded blocks to commit to the state",
                        );
                    }

                    metrics::counter!("sync.max.height.limit.paused.count").increment(1);
                } else if block_height <= lookahead_reset_height && past_lookahead_limit_receiver.cloned_watch_data() {
                    // Reset the watched value to false, since we're well under the limit.
                    // We need to block here, because if we don't the syncer can hang.

                    // But if Zebra is shutting down, ignore the send error.
                    let _ = past_lookahead_limit_sender.lock().expect("thread panicked while holding the past_lookahead_limit_sender mutex guard").send(false);
                    metrics::counter!("sync.max.height.limit.reset.count").increment(1);

                    metrics::counter!("sync.max.height.limit.reset.attempt.count").increment(1);
                }

                if block_height < min_accepted_height {
                    debug!(
                        ?hash,
                        ?block_height,
                        ?tip_height,
                        ?min_accepted_height,
                        behind_tip_limit = ?zs::MAX_BLOCK_REORG_HEIGHT,
                        "synced block height behind the finalized tip: dropped downloaded block"
                    );
                    metrics::counter!("gossip.min.height.limit.dropped.block.count").increment(1);

                    Err(BlockDownloadVerifyError::BehindTipHeightLimit { height: block_height, hash })?;
                }

                // Wait for the verifier service to be ready.
                let readiness = verifier.ready();
                // Prefer the cancel handle if both are ready.
                let verifier = tokio::select! {
                    biased;
                    _ = &mut cancel_rx => {
                        trace!("task cancelled waiting for verifier service readiness");
                        metrics::counter!("sync.cancelled.verify.ready.count").increment(1);
                        return Err(BlockDownloadVerifyError::CancelledAwaitingVerifierReadiness { height: block_height, hash })
                    }
                    verifier = readiness => verifier,
                };

                // Verify the block.
                let verify_start = std::time::Instant::now();
                let mut rsp = verifier
                    .map_err(|error| BlockDownloadVerifyError::VerifierServiceError { error })?
                    .call(zebra_consensus::Request::Commit(block)).boxed();

                // Add a shorter timeout to workaround a known bug (#5125)
                let short_timeout_max = (max_checkpoint_height + FINAL_CHECKPOINT_BLOCK_VERIFY_TIMEOUT_LIMIT).expect("checkpoint block height is in valid range");
                if block_height >= max_checkpoint_height && block_height <= short_timeout_max {
                    rsp = timeout(FINAL_CHECKPOINT_BLOCK_VERIFY_TIMEOUT, rsp)
                        .map_err(|timeout| format!("initial fully verified block timed out: retrying: {timeout:?}").into())
                        .map(|nested_result| nested_result.and_then(convert::identity)).boxed();
                }

                let verification = tokio::select! {
                    biased;
                    _ = &mut cancel_rx => {
                        trace!("task cancelled prior to verification");
                        metrics::counter!("sync.cancelled.verify.count").increment(1);
                        metrics::histogram!("sync.block.verify.duration_seconds", "result" => "cancelled")
                            .record(verify_start.elapsed().as_secs_f64());
                        return Err(BlockDownloadVerifyError::CancelledDuringVerification { height: block_height, hash })
                    }
                    verification = rsp => verification,
                };

                let verify_result = if verification.is_ok() { "success" } else { "failure" };
                metrics::histogram!("sync.block.verify.duration_seconds", "result" => verify_result)
                    .record(verify_start.elapsed().as_secs_f64());

                if verification.is_ok() {
                    metrics::counter!("sync.verified.block.count").increment(1);
                }

                verification
                    .map(|hash| (block_height, hash))
                    .map_err(|err| {
                        match err.downcast::<zebra_consensus::router::RouterError>() {
                            Ok(error) => BlockDownloadVerifyError::Invalid { error: *error, height: block_height, hash, advertiser_addr },
                            Err(error) => BlockDownloadVerifyError::ValidationRequestError { error, height: block_height, hash },
                        }
                    })
            }
            .in_current_span()
            // Tack the hash onto the error so we can remove the cancel handle
            // on failure as well as on success.
            .map_err(move |e| (e, hash)),
        );

        // Try to start the spawned task before queueing the next block request
        tokio::task::yield_now().await;

        self.pending.push(task);
        assert!(
            self.cancel_handles.insert(hash, cancel_tx).is_none(),
            "blocks are only queued once"
        );

        Ok(())
    }

    /// Cancel all running tasks and reset the downloader state.
    pub fn cancel_all(&mut self) {
        // Replace the pending task list with an empty one and drop it.
        let _ = std::mem::take(&mut self.pending);

        // Signal cancellation to all running tasks.
        // Since we already dropped the JoinHandles above, they should
        // fail silently.
        for (_hash, cancel) in self.cancel_handles.drain() {
            let _ = cancel.send(());
        }

        assert!(self.pending.is_empty());
        assert!(self.cancel_handles.is_empty());

        // Set the lookahead limit to false, since we're empty (so we're under the limit).
        //
        // It is ok to block here, because we're doing a reset and sleep anyway.
        // But if Zebra is shutting down, ignore the send error.
        let _ = self
            .past_lookahead_limit_sender
            .lock()
            .expect("thread panicked while holding the past_lookahead_limit_sender mutex guard")
            .send(false);
    }

    /// Get the number of currently in-flight download and verify tasks.
    pub fn in_flight(&mut self) -> usize {
        self.pending.len()
    }

    /// Returns true if there are no in-flight download and verify tasks.
    #[allow(dead_code)]
    pub fn is_empty(&mut self) -> bool {
        self.pending.is_empty()
    }
}
