//! A download stream for Zebra's block syncer.

use std::{
    collections::HashMap,
    convert::TryFrom,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

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
    block::{self, Block, Height},
    chain_tip::ChainTip,
};
use zebra_network as zn;
use zebra_state as zs;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// A multiplier used to calculate the extra number of blocks we allow in the
/// verifier and state pipelines, on top of the lookahead limit.
///
/// The extra number of blocks is calculated using
/// `lookahead_limit * VERIFICATION_PIPELINE_SCALING_MULTIPLIER`.
///
/// This allows the verifier and state queues to hold a few extra tips responses worth of blocks,
/// even if the syncer queue is full. Any unused capacity is shared between both queues.
///
/// If this capacity is exceeded, the downloader will start failing download blocks with
/// [`BlockDownloadVerifyError::AboveLookaheadHeightLimit`], and the syncer will reset.
///
/// Since the syncer queue is limited to the `lookahead_limit`,
/// the rest of the capacity is reserved for the other queues.
/// There is no reserved capacity for the syncer queue:
/// if the other queues stay full, the syncer will eventually time out and reset.
const VERIFICATION_PIPELINE_SCALING_MULTIPLIER: usize = 2;

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
        error: zebra_consensus::chain::VerifyChainError,
        height: block::Height,
        hash: block::Hash,
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
}

/// Represents a [`Stream`] of download and verification tasks during chain sync.
#[pin_project]
#[derive(Debug)]
pub struct Downloads<ZN, ZV, ZSTip>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Sync + 'static,
    ZN::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZV::Future: Send,
    ZSTip: ChainTip + Clone + Send + 'static,
{
    // Services
    /// A service that forwards requests to connected peers, and returns their
    /// responses.
    network: ZN,

    /// A service that verifies downloaded blocks.
    verifier: ZV,

    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: ZSTip,

    // Configuration
    /// The configured lookahead limit, after applying the minimum limit.
    lookahead_limit: usize,

    // Internal downloads state
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
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError>
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
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZV::Future: Send,
    ZSTip: ChainTip + Clone + Send + 'static,
{
    /// Initialize a new download stream with the provided `network` and
    /// `verifier` services. Uses the `latest_chain_tip` and `lookahead_limit`
    /// to drop blocks that are too far ahead of the current state tip.
    ///
    /// The [`Downloads`] stream is agnostic to the network policy, so retry and
    /// timeout limits should be applied to the `network` service passed into
    /// this constructor.
    pub fn new(network: ZN, verifier: ZV, latest_chain_tip: ZSTip, lookahead_limit: usize) -> Self {
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
    pub async fn download_and_verify(
        &mut self,
        hash: block::Hash,
    ) -> Result<(), BlockDownloadVerifyError> {
        if self.cancel_handles.contains_key(&hash) {
            metrics::counter!("sync.already.queued.dropped.block.hash.count", 1);
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

        let task = tokio::spawn(
            async move {
                // Download the block.
                // Prefer the cancel handle if both are ready.
                let rsp = tokio::select! {
                    biased;
                    _ = &mut cancel_rx => {
                        trace!("task cancelled prior to download completion");
                        metrics::counter!("sync.cancelled.download.count", 1);
                        return Err(BlockDownloadVerifyError::CancelledDuringDownload { hash })
                    }
                    rsp = block_req => rsp.map_err(|error| BlockDownloadVerifyError::DownloadFailed { error, hash})?,
                };

                let block = if let zn::Response::Blocks(blocks) = rsp {
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
                metrics::counter!("sync.downloaded.block.count", 1);

                // Security & Performance: reject blocks that are too far ahead of our tip.
                // Avoids denial of service attacks, and reduces wasted work on high blocks
                // that will timeout before being verified.
                let tip_height = latest_chain_tip.best_tip_height();

                // TODO: don't use VERIFICATION_PIPELINE_SCALING_MULTIPLIER for full verification?
                let max_lookahead_height = if let Some(tip_height) = tip_height {
                    // Scale the height limit with the lookahead limit,
                    // so users with low capacity or under DoS can reduce them both.
                    let lookahead = i32::try_from(
                        lookahead_limit + lookahead_limit * VERIFICATION_PIPELINE_SCALING_MULTIPLIER,
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

                let block_height = if let Some(block_height) = block.coinbase_height() {
                    block_height
                } else {
                    debug!(
                        ?hash,
                        "synced block with no height: dropped downloaded block"
                    );
                    metrics::counter!("sync.no.height.dropped.block.count", 1);

                    return Err(BlockDownloadVerifyError::InvalidHeight { hash });
                };

                if block_height > max_lookahead_height {
                    info!(
                        ?hash,
                        ?block_height,
                        ?tip_height,
                        ?max_lookahead_height,
                        lookahead_limit = ?lookahead_limit,
                        "synced block height too far ahead of the tip: dropped downloaded block",
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
                    Err(BlockDownloadVerifyError::AboveLookaheadHeightLimit { height: block_height, hash })?;
                } else if block_height < min_accepted_height {
                    debug!(
                        ?hash,
                        ?block_height,
                        ?tip_height,
                        ?min_accepted_height,
                        behind_tip_limit = ?zs::MAX_BLOCK_REORG_HEIGHT,
                        "synced block height behind the finalized tip: dropped downloaded block"
                    );
                    metrics::counter!("gossip.min.height.limit.dropped.block.count", 1);

                    Err(BlockDownloadVerifyError::BehindTipHeightLimit { height: block_height, hash })?;
                }

                // Wait for the verifier service to be ready.
                let readiness = verifier.ready();
                // Prefer the cancel handle if both are ready.
                let verifier = tokio::select! {
                    biased;
                    _ = &mut cancel_rx => {
                        trace!("task cancelled waiting for verifier service readiness");
                        metrics::counter!("sync.cancelled.verify.ready.count", 1);
                        return Err(BlockDownloadVerifyError::CancelledAwaitingVerifierReadiness { height: block_height, hash })
                    }
                    verifier = readiness => verifier,
                };

                // Verify the block.
                let rsp = verifier
                    .map_err(|error| BlockDownloadVerifyError::VerifierServiceError { error })?
                    .call(block);

                let verification = tokio::select! {
                    biased;
                    _ = &mut cancel_rx => {
                        trace!("task cancelled prior to verification");
                        metrics::counter!("sync.cancelled.verify.count", 1);
                        return Err(BlockDownloadVerifyError::CancelledDuringVerification { height: block_height, hash })
                    }
                    verification = rsp => verification,
                };

                if verification.is_ok() {
                    metrics::counter!("sync.verified.block.count", 1);
                }

                verification
                    .map(|hash| (block_height, hash))
                    .map_err(|err| {
                        match err.downcast::<zebra_consensus::chain::VerifyChainError>() {
                            Ok(error) => BlockDownloadVerifyError::Invalid { error: *error, height: block_height, hash },
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
    }

    /// Get the number of currently in-flight download tasks.
    pub fn in_flight(&mut self) -> usize {
        self.pending.len()
    }
}
