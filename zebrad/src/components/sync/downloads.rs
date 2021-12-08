use std::{
    collections::HashMap,
    convert::TryFrom,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use color_eyre::eyre::{eyre, Report};
use futures::{
    future::TryFutureExt,
    ready,
    stream::{FuturesUnordered, Stream},
};
use pin_project::pin_project;
use thiserror::Error;
use tokio::{sync::oneshot, task::JoinHandle};
use tower::{hedge, Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::{
    block::{self, Block},
    chain_tip::ChainTip,
};
use zebra_network as zn;
use zebra_state as zs;

use super::{DEFAULT_LOOKAHEAD_LIMIT, MAX_TIPS_RESPONSE_HASH_COUNT};

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// A divisor used to calculate the extra number of blocks we allow in the
/// verifier and state pipelines, on top of the lookahead limit.
///
/// The extra number of blocks is calculated using
/// `lookahead_limit / VERIFICATION_PIPELINE_SCALING_DIVISOR`.
///
/// For the default lookahead limit, the extra number of blocks is
/// `2 * MAX_TIPS_RESPONSE_HASH_COUNT`.
///
/// This allows the verifier and state queues to hold an extra two tips responses worth of blocks,
/// even if the syncer queue is full. Any unused capacity is shared between both queues.
///
/// Since the syncer queue is limited to the `lookahead_limit`,
/// the rest of the capacity is reserved for the other queues.
/// There is no reserved capacity for the syncer queue:
/// if the other queues stay full, the syncer will eventually time out and reset.
const VERIFICATION_PIPELINE_SCALING_DIVISOR: usize =
    DEFAULT_LOOKAHEAD_LIMIT / (2 * MAX_TIPS_RESPONSE_HASH_COUNT);

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
    #[error("error downloading block")]
    DownloadFailed(#[source] BoxError),

    #[error("error from the verifier service")]
    VerifierError(#[source] BoxError),

    #[error("block did not pass consensus validation")]
    Invalid(#[from] zebra_consensus::chain::VerifyChainError),

    #[error("downloaded block was too far ahead of the chain tip")]
    AboveLookaheadHeightLimit,

    #[error("downloaded block was too far behind the chain tip")]
    BehindTipHeightLimit,

    #[error("downloaded block had an invalid height")]
    InvalidHeight,

    #[error("block download / verification was cancelled during download")]
    CancelledDuringDownload,

    #[error("block download / verification was cancelled during verification")]
    CancelledDuringVerification,
}

/// Represents a [`Stream`] of download and verification tasks during chain sync.
#[pin_project]
#[derive(Debug)]
pub struct Downloads<ZN, ZV>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + 'static,
    ZN::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
{
    // Services
    /// A service that forwards requests to connected peers, and returns their
    /// responses.
    network: ZN,

    /// A service that verifies downloaded blocks.
    verifier: ZV,

    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: zs::LatestChainTip,

    // Configuration
    /// The configured lookahead limit, after applying the minimum limit.
    lookahead_limit: usize,

    // Internal downloads state
    /// A list of pending block download and verify tasks.
    #[pin]
    pending:
        FuturesUnordered<JoinHandle<Result<block::Hash, (BlockDownloadVerifyError, block::Hash)>>>,

    /// A list of channels that can be used to cancel pending block download and
    /// verify tasks.
    cancel_handles: HashMap<block::Hash, oneshot::Sender<()>>,
}

impl<ZN, ZV> Stream for Downloads<ZN, ZV>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + 'static,
    ZN::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
{
    type Item = Result<block::Hash, BlockDownloadVerifyError>;

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
                Ok(hash) => {
                    this.cancel_handles.remove(&hash);
                    Poll::Ready(Some(Ok(hash)))
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

impl<ZN, ZV> Downloads<ZN, ZV>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + 'static,
    ZN::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
{
    /// Initialize a new download stream with the provided `network` and
    /// `verifier` services. Uses the `latest_chain_tip` and `lookahead_limit`
    /// to drop blocks that are too far ahead of the current state tip.
    ///
    /// The [`Downloads`] stream is agnostic to the network policy, so retry and
    /// timeout limits should be applied to the `network` service passed into
    /// this constructor.
    pub fn new(
        network: ZN,
        verifier: ZV,
        latest_chain_tip: zs::LatestChainTip,
        lookahead_limit: usize,
    ) -> Self {
        Self {
            network,
            verifier,
            latest_chain_tip,
            lookahead_limit,
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
    pub async fn download_and_verify(&mut self, hash: block::Hash) -> Result<(), Report> {
        if self.cancel_handles.contains_key(&hash) {
            metrics::counter!("sync.already.queued.dropped.block.hash.count", 1);
            return Err(eyre!("duplicate hash queued for download: {:?}", hash));
        }

        // We construct the block requests sequentially, waiting for the peer
        // set to be ready to process each request. This ensures that we start
        // block downloads in the order we want them (though they may resolve
        // out of order), and it means that we respect backpressure. Otherwise,
        // if we waited for readiness and did the service call in the spawned
        // tasks, all of the spawned tasks would race each other waiting for the
        // network to become ready.
        tracing::debug!("waiting to request block");
        let block_req = self
            .network
            .ready()
            .await
            .map_err(|e| eyre!(e))?
            .call(zn::Request::BlocksByHash(std::iter::once(hash).collect()));
        tracing::debug!("requested block");

        // This oneshot is used to signal cancellation to the download task.
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();

        let mut verifier = self.verifier.clone();
        let latest_chain_tip = self.latest_chain_tip.clone();
        let lookahead_limit = self.lookahead_limit;

        let task = tokio::spawn(
            async move {
                // Prefer the cancel handle if both are ready.
                let rsp = tokio::select! {
                    biased;
                    _ = &mut cancel_rx => {
                        tracing::trace!("task cancelled prior to download completion");
                        metrics::counter!("sync.cancelled.download.count", 1);
                        return Err(BlockDownloadVerifyError::CancelledDuringDownload)
                    }
                    rsp = block_req => rsp.map_err(BlockDownloadVerifyError::DownloadFailed)?,
                };

                let block = if let zn::Response::Blocks(blocks) = rsp {
                    assert_eq!(
                        blocks.len(),
                        1,
                        "wrong number of blocks in response to a single hash"
                    );

                    blocks
                        .into_iter()
                        .next()
                        .expect("successful response has the block in it")
                } else {
                    unreachable!("wrong response to block request");
                };
                metrics::counter!("sync.downloaded.block.count", 1);

                // Security & Performance: reject blocks that are too far ahead of our tip.
                // Avoids denial of service attacks, and reduces wasted work on high blocks
                // that will timeout before being verified.
                let tip_height = latest_chain_tip.best_tip_height();

                let max_lookahead_height = if let Some(tip_height) = tip_height {
                    // Scale the height limit with the lookahead limit,
                    // so users with low capacity or under DoS can reduce them both.
                    let lookahead = i32::try_from(
                        lookahead_limit + lookahead_limit / VERIFICATION_PIPELINE_SCALING_DIVISOR,
                    )
                    .expect("fits in i32");
                    (tip_height + lookahead).expect("tip is much lower than Height::MAX")
                } else {
                    let genesis_lookahead =
                        u32::try_from(lookahead_limit - 1).expect("fits in u32");
                    block::Height(genesis_lookahead)
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

                if let Some(block_height) = block.coinbase_height() {
                    if block_height > max_lookahead_height {
                        tracing::info!(
                            ?hash,
                            ?block_height,
                            ?tip_height,
                            ?max_lookahead_height,
                            lookahead_limit = ?lookahead_limit,
                            "synced block height too far ahead of the tip: dropped downloaded block"
                        );
                        metrics::counter!("sync.max.height.limit.dropped.block.count", 1);

                        // This error should be very rare during normal operation.
                        //
                        // We need to reset the syncer on this error,
                        // to allow the verifier and state to catch up,
                        // or prevent it following a bad chain.
                        //
                        // If we don't reset the syncer on this error,
                        // it will continue downloading blocks from a bad chain,
                        // (or blocks far ahead of the current state tip).
                        Err(BlockDownloadVerifyError::AboveLookaheadHeightLimit)?;
                    } else if block_height < min_accepted_height {
                        tracing::info!(
                            ?hash,
                            ?block_height,
                            ?tip_height,
                            ?min_accepted_height,
                            behind_tip_limit = ?zs::MAX_BLOCK_REORG_HEIGHT,
                            "synced block height behind the finalized tip: dropped downloaded block"
                        );
                        metrics::counter!("gossip.min.height.limit.dropped.block.count", 1);

                        Err(BlockDownloadVerifyError::BehindTipHeightLimit)?;
                    }
                } else {
                    tracing::info!(
                        ?hash,
                        "synced block with no height: dropped downloaded block"
                    );
                    metrics::counter!("sync.no.height.dropped.block.count", 1);

                    Err(BlockDownloadVerifyError::InvalidHeight)?;
                }

                let rsp = verifier
                    .ready()
                    .await
                    .map_err(BlockDownloadVerifyError::VerifierError)?
                    .call(block);
                // Prefer the cancel handle if both are ready.
                let verification = tokio::select! {
                    biased;
                    _ = &mut cancel_rx => {
                        tracing::trace!("task cancelled prior to verification");
                        metrics::counter!("sync.cancelled.verify.count", 1);
                        return Err(BlockDownloadVerifyError::CancelledDuringVerification)
                    }
                    verification = rsp => verification,
                };
                if verification.is_ok() {
                    metrics::counter!("sync.verified.block.count", 1);
                }

                verification.map_err(|err| {
                    match err.downcast::<zebra_consensus::chain::VerifyChainError>() {
                        Ok(e) => BlockDownloadVerifyError::Invalid(*e),
                        Err(e) => BlockDownloadVerifyError::VerifierError(e),
                    }
                })
            }
            .in_current_span()
            // Tack the hash onto the error so we can remove the cancel handle
            // on failure as well as on success.
            .map_err(move |e| (e, hash)),
        );

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
    }

    /// Get the number of currently in-flight download tasks.
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }
}
