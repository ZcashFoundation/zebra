//! A download stream that handles gossiped blocks from peers.

#[cfg(test)]
mod tests;

use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use futures::{
    future::TryFutureExt,
    ready,
    stream::{FuturesUnordered, Stream},
};
use pin_project::pin_project;
use thiserror::Error;
use tokio::{sync::oneshot, task::JoinHandle, time::timeout};
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

/// How long to wait for the parent-height lookup that decides whether a behind-tip gossiped block
/// was forged.
///
/// Bounds a drop path that also fires for honest old blocks. Timing out is treated as "no proof",
/// so a slow state read costs the supplying peer nothing.
const PARENT_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// A gossiped block was dropped before verification because its coinbase height was outside the
/// accepted range around the chain tip.
///
/// Peers legitimately serve blocks that are genuinely far ahead of the tip while Zebra is catching
/// up, and blocks that are genuinely older than the finalized tip, so this error is returned for
/// every such drop. But a peer can also answer a [`BlocksByHash`](zn::Request::BlocksByHash) request
/// with a canonical header and a rewritten coinbase height, because the coinbase scriptSig is
/// excluded from the V5 transaction ID and therefore from the block hash (ZIP-244). The initial hash
/// check still passes, so the forged height reaches these drops before consensus validation, and the
/// verifier never scores the peer (GHSA-4f6v-mj46-gxg3).
///
/// The downloader attaches the supplying peer's address to the drop only when the parent header
/// Zebra already holds proves the claimed height wrong (see [`advertiser_if_parent_contradicts`]),
/// so the inbound handler scores a proven rewrite, and never an authentic block.
///
/// This is the inbound-gossip sibling of the sync path's height limit errors (GHSA-g95h-hw6g-pvgv).
#[derive(Copy, Clone, Debug, Error)]
pub enum HeightLimitError {
    /// The block's coinbase height is above the lookahead limit.
    #[error("gossiped block height {height:?} too far ahead of the tip: {hash:?}")]
    AboveLookahead {
        height: block::Height,
        hash: block::Hash,
    },

    /// The block's coinbase height is behind the finalized tip.
    #[error("gossiped block height {height:?} behind the finalized tip: {hash:?}")]
    BehindTip {
        height: block::Height,
        hash: block::Hash,
    },
}

impl HeightLimitError {
    /// The misbehavior score for a gossiped block whose parent proves its height was rewritten.
    ///
    /// A rewritten coinbase height is unambiguous misbehavior whichever limit it crossed, so score it
    /// at the ban threshold, matching the sync path (GHSA-g95h-hw6g-pvgv).
    pub fn misbehavior_score(&self) -> u32 {
        zn::constants::MAX_PEER_MISBEHAVIOR_SCORE
    }
}

/// Returns `advertiser_addr` if the parent header Zebra already holds proves that a gossiped block's
/// claimed `block_height` was rewritten, and `None` if there is no such proof.
///
/// # Security
///
/// A peer can answer a `BlocksByHash` request with a canonical header and a rewritten coinbase
/// height, because the coinbase scriptSig is excluded from the V5 transaction ID and therefore from
/// the block hash (ZIP-244). The hash check passes, so the forged height reaches the height limit
/// drops before consensus validation. Peers also legitimately serve blocks that are genuinely far
/// ahead of the tip or genuinely older than the finalized tip, so the drop is only attributed to the
/// supplying peer when the parent header Zebra holds contradicts the claimed height: a block's height
/// is one more than its parent's, so a held parent whose height disagrees proves the body was
/// rewritten, whichever limit the claimed height crossed.
///
/// A parent Zebra does not hold, a height consistent with the parent, and a failed or timed-out
/// lookup are all treated as no proof, so honest peers are never scored and a slow state read costs
/// the peer nothing. (GHSA-4f6v-mj46-gxg3, the inbound-gossip sibling of GHSA-g95h-hw6g-pvgv.)
async fn advertiser_if_parent_contradicts<ZS>(
    state: ZS,
    parent_hash: block::Hash,
    block_height: block::Height,
    advertiser_addr: Option<PeerSocketAddr>,
) -> Option<PeerSocketAddr>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send,
{
    // There is no peer to score, so skip the state lookup.
    let advertiser_addr = advertiser_addr?;

    match timeout(
        PARENT_LOOKUP_TIMEOUT,
        state.oneshot(zs::Request::BlockHeader(parent_hash.into())),
    )
    .await
    {
        Ok(Ok(zs::Response::BlockHeader {
            height: parent_height,
            ..
        })) if (parent_height + 1) != Some(block_height) => Some(advertiser_addr),
        // Parent unknown, height consistent with it, or the lookup failed or timed out: there is no
        // proof of misbehavior, so the peer is not scored.
        _ => None,
    }
}

/// The maximum number of concurrent inbound download and verify tasks.
/// Also used as the maximum lookahead limit, before block verification.
///
/// We expect the syncer to download and verify checkpoints, so this bound
/// can be small.
///
/// ## Security
///
/// The maximum block size is 2 million bytes. A deserialized malicious
/// block with ~225_000 transparent outputs can take up 9MB of RAM.
/// The total queue bound is `MAX_INBOUND_CONCURRENCY * 9 MB`. Each peer IP
/// is limited to one in-flight download (9 MB) by the per-IP cap enforced
/// in [`Downloads::download_and_verify`], so a sybil or IPv6-range attacker
/// still needs many distinct source IPs to approach the total bound.
/// (See #1880 for more details.)
///
/// Malicious blocks will eventually timeout or fail contextual validation.
/// Once validation fails, the block is dropped, and its memory is deallocated.
pub const MAX_INBOUND_CONCURRENCY: usize = 200;

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

    /// The advertising peer's IP already has an in-flight download, so
    /// this request was ignored. Zcash's post-Blossom target block spacing
    /// is 75 seconds, so honest peers rarely gossip more than one block
    /// before the first is verified; during reorgs or recovery the same
    /// hash also arrives from other peers or via the syncer.
    TooManyFromPeer,
}

/// Manages download and verification of blocks gossiped to this peer.
#[pin_project]
#[derive(Debug)]
pub struct Downloads<ZN, ZV, ZS, ZSTip>
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
    ZSTip: ChainTip + Clone + Send + 'static,
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
    latest_chain_tip: ZSTip,

    // Internal downloads state
    //
    /// A list of pending block download and verify tasks.
    #[pin]
    pending: FuturesUnordered<
        JoinHandle<Result<block::Hash, (BoxError, block::Hash, Option<PeerSocketAddr>)>>,
    >,

    /// Cancellation handles for tasks in [`Self::pending`], keyed by block
    /// hash. The `Option<IpAddr>` is the advertiser IP recorded in
    /// [`Self::in_flight_ips`], so completion can remove it by hash lookup.
    cancel_handles: HashMap<block::Hash, (oneshot::Sender<()>, Option<IpAddr>)>,

    /// Advertiser IPs with an in-flight download and verify task.
    ///
    /// Invariant: an IP is present iff some entry in [`Self::cancel_handles`]
    /// has value `(_, Some(ip))`. Enforces the one-download-per-IP cap.
    ///
    /// Size-bounded by `full_verify_concurrency_limit` (≤ [`MAX_INBOUND_CONCURRENCY`]),
    /// inherited from the [`DownloadAction::FullQueue`] check on
    /// [`Self::pending`].
    in_flight_ips: HashSet<IpAddr>,
}

impl<ZN, ZV, ZS, ZSTip> Stream for Downloads<ZN, ZV, ZS, ZSTip>
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
    ZSTip: ChainTip + Clone + Send + 'static,
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
            let (result, hash) =
                match join_result.expect("block download and verify tasks must not panic") {
                    Ok(hash) => (Ok(hash), hash),
                    Err((e, hash, advertiser_addr)) => (Err((e, advertiser_addr)), hash),
                };
            if let Some((_, Some(ip))) = this.cancel_handles.remove(&hash) {
                assert!(
                    this.in_flight_ips.remove(&ip),
                    "every tracked IP was inserted when its download was queued",
                );
            }
            Poll::Ready(Some(result))
        } else {
            Poll::Ready(None)
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.pending.size_hint()
    }
}

impl<ZN, ZV, ZS, ZSTip> Downloads<ZN, ZV, ZS, ZSTip>
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
    ZSTip: ChainTip + Clone + Send + 'static,
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
        latest_chain_tip: ZSTip,
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
            in_flight_ips: HashSet::new(),
        }
    }

    /// Queue a block for download and verification.
    ///
    /// When `advertiser` is `Some`, its IP is tracked in
    /// [`Self::in_flight_ips`] and used to enforce the one-download-per-IP
    /// cap; `None` bypasses per-IP accounting (for example when Zebra
    /// triggers the download internally).
    #[instrument(skip(self, hash), fields(hash = %hash))]
    pub fn download_and_verify(
        &mut self,
        hash: block::Hash,
        advertiser: Option<PeerSocketAddr>,
    ) -> DownloadAction {
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

        let advertiser_ip = advertiser.map(|addr| addr.ip());
        if let Some(ip) = advertiser_ip {
            if self.in_flight_ips.contains(&ip) {
                debug!(
                    ?hash,
                    ?advertiser,
                    "already have an in-flight inbound download from peer IP: ignored block",
                );

                metrics::counter!("gossip.peer.limit.dropped.block.hash.count").increment(1);

                return DownloadAction::TooManyFromPeer;
            }
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
            match state.clone().oneshot(zs::Request::KnownBlock(hash)).await {
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

                // # Security
                //
                // Attribute the drop to the supplying peer only if the parent Zebra holds proves the
                // claimed height was rewritten (GHSA-4f6v-mj46-gxg3). A genuinely far-ahead block
                // has a parent Zebra does not hold yet, so it is dropped anonymously.
                let advertiser_addr = advertiser_if_parent_contradicts(
                    state,
                    block.header.previous_block_hash,
                    block_height,
                    advertiser_addr,
                )
                .await;

                return Err((
                    BoxError::from(HeightLimitError::AboveLookahead {
                        height: block_height,
                        hash,
                    }),
                    advertiser_addr,
                ));
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

                // # Security
                //
                // Attribute the drop to the supplying peer only if the parent Zebra holds proves the
                // claimed height was rewritten (GHSA-4f6v-mj46-gxg3). A genuinely old block is
                // consistent with its parent, or has a parent Zebra does not hold, so it is dropped
                // anonymously.
                let advertiser_addr = advertiser_if_parent_contradicts(
                    state,
                    block.header.previous_block_hash,
                    block_height,
                    advertiser_addr,
                )
                .await;

                return Err((
                    BoxError::from(HeightLimitError::BehindTip {
                        height: block_height,
                        hash,
                    }),
                    advertiser_addr,
                ));
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
        // Tack the hash onto the error so poll_next can look up the cancel
        // handle and advertising IP on failure as well as success.
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
            self.cancel_handles
                .insert(hash, (cancel_tx, advertiser_ip))
                .is_none(),
            "blocks are only queued once"
        );
        if let Some(ip) = advertiser_ip {
            assert!(
                self.in_flight_ips.insert(ip),
                "the per-IP cap check above rejects any IP already in flight",
            );
        }

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
