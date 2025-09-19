//! A download stream that handles gossiped blocks from peers.

use std::{
    collections::HashMap,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{
    future::TryFutureExt,
    ready,
    stream::{FuturesUnordered, Stream},
};
use pin_project::pin_project;
use tokio::{sync::oneshot, task::JoinHandle};
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::{
    block::{self, HeightDiff},
    chain_tip::ChainTip,
};
use zebra_network::{self as zn, PeerSocketAddr};
use zebra_state as zs;

use crate::components::sync::MIN_CONCURRENCY_LIMIT;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// The maximum number of concurrent inbound download and verify tasks.
/// Also used as the maximum lookahead limit, before block verification.
///
/// We expect the syncer to download and verify checkpoints, so this bound
/// can be small.
///
/// ## Security
///
/// We use a small concurrency limit, to prevent memory denial-of-service
/// attacks.
///
/// The maximum block size is 2 million bytes. A deserialized malicious
/// block with ~225_000 transparent outputs can take up 9MB of RAM.
/// So the maximum inbound queue usage is `MAX_INBOUND_CONCURRENCY * 9 MB`.
/// (See #1880 for more details.)
///
/// Malicious blocks will eventually timeout or fail contextual validation.
/// Once validation fails, the block is dropped, and its memory is deallocated.
///
/// Since Zebra keeps an `inv` index, inbound downloads for malicious blocks
/// will be directed to the malicious node that originally gossiped the hash.
/// Therefore, this attack can be carried out by a single malicious node.
pub const MAX_INBOUND_CONCURRENCY: usize = 30;

/// The action taken in response to a peer's gossiped block hash.
pub enum DownloadAction {
    /// The block hash was successfully queued for download and verification.
    AddedToQueue,

    /// The block hash is already queued, so this request was ignored.
    ///
    /// Another peer has already gossiped the same hash to us.
    AlreadyQueued,

    /// The queue is at capacity, so this request was ignored.
    ///
    /// The sync service should discover this block later, when we are closer
    /// to the tip. The queue's capacity is [`Downloads::full_verify_concurrency_limit`].
    FullQueue,
}

/// Manages download and verification of blocks gossiped to this peer.
#[pin_project]
#[derive(Debug)]
pub struct Downloads<ZN, ZV, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Clone
        + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send,
{
    // Configuration
    //
    /// The configured full verification concurrency limit, after applying the minimum limit.
    full_verify_concurrency_limit: usize,

    // Services
    //
    /// A service that forwards requests to connected peers, and returns their
    /// responses.
    network: ZN,

    /// A service that verifies downloaded blocks.
    verifier: ZV,

    /// A service that manages cached blockchain state.
    state: ZS,

    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: zs::LatestChainTip,

    // Internal downloads state
    //
    /// A list of pending block download and verify tasks.
    #[pin]
    pending: FuturesUnordered<
        JoinHandle<Result<block::Hash, (BoxError, block::Hash, Option<PeerSocketAddr>)>>,
    >,

    /// A list of channels that can be used to cancel pending block download and
    /// verify tasks.
    cancel_handles: HashMap<block::Hash, oneshot::Sender<()>>,
}

impl<ZN, ZV, ZS> Stream for Downloads<ZN, ZV, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Clone
        + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send,
{
    type Item = Result<block::Hash, (BoxError, Option<PeerSocketAddr>)>;

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
                Err((e, hash, advertiser_addr)) => {
                    this.cancel_handles.remove(&hash);
                    Poll::Ready(Some(Err((e, advertiser_addr))))
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

impl<ZN, ZV, ZS> Downloads<ZN, ZV, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Clone
        + 'static,
    ZV::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send,
{
    /// Initialize a new download stream with the provided `network`, `verifier`, and `state` services.
    /// The `latest_chain_tip` must be linked to the provided `state` service.
    ///
    /// The [`Downloads`] stream is agnostic to the network policy, so retry and
    /// timeout limits should be applied to the `network` service passed into
    /// this constructor.
    pub fn new(
        full_verify_concurrency_limit: usize,
        network: ZN,
        verifier: ZV,
        state: ZS,
        latest_chain_tip: zs::LatestChainTip,
    ) -> Self {
        // The syncer already warns about the minimum.
        let full_verify_concurrency_limit =
            full_verify_concurrency_limit.clamp(MIN_CONCURRENCY_LIMIT, MAX_INBOUND_CONCURRENCY);

        Self {
            full_verify_concurrency_limit,
            network,
            verifier,
            state,
            latest_chain_tip,
            pending: FuturesUnordered::new(),
            cancel_handles: HashMap::new(),
        }
    }

    /// Queue a block for download and verification.
    ///
    /// Returns the action taken in response to the queue request.
    #[instrument(skip(self, hash), fields(hash = %hash))]
    pub fn download_and_verify(&mut self, hash: block::Hash) -> DownloadAction {
        if self.cancel_handles.contains_key(&hash) {
            debug!(
                ?hash,
                queue_len = self.pending.len(),
                concurrency_limit = self.full_verify_concurrency_limit,
                "block hash already queued for inbound download: ignored block",
            );

            metrics::gauge!("gossip.queued.block.count").set(self.pending.len() as f64);
            metrics::counter!("gossip.already.queued.dropped.block.hash.count").increment(1);

            return DownloadAction::AlreadyQueued;
        }

        if self.pending.len() >= self.full_verify_concurrency_limit {
            debug!(
                ?hash,
                queue_len = self.pending.len(),
                concurrency_limit = self.full_verify_concurrency_limit,
                "too many blocks queued for inbound download: ignored block",
            );

            metrics::gauge!("gossip.queued.block.count").set(self.pending.len() as f64);
            metrics::counter!("gossip.full.queue.dropped.block.hash.count").increment(1);

            return DownloadAction::FullQueue;
        }

        // This oneshot is used to signal cancellation to the download task.
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();

        let state = self.state.clone();
        let network = self.network.clone();
        let verifier = self.verifier.clone();
        let latest_chain_tip = self.latest_chain_tip.clone();
        let full_verify_concurrency_limit = self.full_verify_concurrency_limit;

        let fut = async move {
            // Check if the block is already in the state.
            match state.oneshot(zs::Request::KnownBlock(hash)).await {
                Ok(zs::Response::KnownBlock(None)) => Ok(()),
                Ok(zs::Response::KnownBlock(Some(_))) => Err("already present".into()),
                Ok(_) => unreachable!("wrong response"),
                Err(e) => Err(e),
            }
            .map_err(|e| (e, None))?;

            let (block, advertiser_addr) = if let zn::Response::Blocks(blocks) = network
                .oneshot(zn::Request::BlocksByHash(std::iter::once(hash).collect()))
                .await
                .map_err(|e| (e, None))?
            {
                assert_eq!(
                    blocks.len(),
                    1,
                    "wrong number of blocks in response to a single hash",
                );

                blocks
                    .first()
                    .expect("just checked length")
                    .available()
                    .expect(
                        "unexpected missing block status: single block failures should be errors",
                    )
            } else {
                unreachable!("wrong response to block request");
            };
            metrics::counter!("gossip.downloaded.block.count").increment(1);

            // # Security & Performance
            //
            // Reject blocks that are too far ahead of our tip,
            // and blocks that are behind the finalized tip.
            //
            // Avoids denial of service attacks. Also reduces wasted work on high blocks
            // that will timeout before being verified, and low blocks that can never be finalized.
            let tip_height = latest_chain_tip.best_tip_height();

            let max_lookahead_height = if let Some(tip_height) = tip_height {
                let lookahead = HeightDiff::try_from(full_verify_concurrency_limit)
                    .expect("fits in HeightDiff");
                (tip_height + lookahead).expect("tip is much lower than Height::MAX")
            } else {
                let genesis_lookahead =
                    u32::try_from(full_verify_concurrency_limit - 1).expect("fits in u32");
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

            let block_height = block
                .coinbase_height()
                .ok_or_else(|| {
                    debug!(
                        ?hash,
                        "gossiped block with no height: dropped downloaded block"
                    );
                    metrics::counter!("gossip.no.height.dropped.block.count").increment(1);

                    BoxError::from("gossiped block with no height")
                })
                .map_err(|e| (e, None))?;

            if block_height > max_lookahead_height {
                debug!(
                    ?hash,
                    ?block_height,
                    ?tip_height,
                    ?max_lookahead_height,
                    lookahead_limit = full_verify_concurrency_limit,
                    "gossiped block height too far ahead of the tip: dropped downloaded block",
                );
                metrics::counter!("gossip.max.height.limit.dropped.block.count").increment(1);

                Err("gossiped block height too far ahead").map_err(|e| (e.into(), None))?;
            } else if block_height < min_accepted_height {
                debug!(
                    ?hash,
                    ?block_height,
                    ?tip_height,
                    ?min_accepted_height,
                    behind_tip_limit = ?zs::MAX_BLOCK_REORG_HEIGHT,
                    "gossiped block height behind the finalized tip: dropped downloaded block",
                );
                metrics::counter!("gossip.min.height.limit.dropped.block.count").increment(1);

                Err("gossiped block height behind the finalized tip")
                    .map_err(|e| (e.into(), None))?;
            }

            verifier
                .oneshot(zebra_consensus::Request::Commit(block))
                .await
                .map(|hash| (hash, block_height))
                .map_err(|e| (e, advertiser_addr))
        }
        .map_ok(|(hash, height)| {
            info!(?height, "downloaded and verified gossiped block");
            metrics::counter!("gossip.verified.block.count").increment(1);
            hash
        })
        // Tack the hash onto the error so we can remove the cancel handle
        // on failure as well as on success.
        .map_err(move |(e, advertiser_addr)| (e, hash, advertiser_addr))
        .in_current_span();

        let task = tokio::spawn(async move {
            // Prefer the cancel handle if both are ready.
            tokio::select! {
                biased;
                _ = &mut cancel_rx => {
                    trace!("task cancelled prior to completion");
                    metrics::counter!("gossip.cancelled.count").increment(1);
                    Err(("canceled".into(), hash, None))
                }
                verification = fut => verification,
            }
        });

        self.pending.push(task);
        assert!(
            self.cancel_handles.insert(hash, cancel_tx).is_none(),
            "blocks are only queued once"
        );

        debug!(
            ?hash,
            queue_len = self.pending.len(),
            concurrency_limit = self.full_verify_concurrency_limit,
            "queued hash for download",
        );
        metrics::gauge!("gossip.queued.block.count").set(self.pending.len() as f64);

        DownloadAction::AddedToQueue
    }
}
