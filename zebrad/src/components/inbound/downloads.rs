//! A download stream that handles gossiped blocks from peers.

#[cfg(test)]
mod tests;

use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use futures::{
    future::TryFutureExt,
    ready,
    stream::{FuturesUnordered, Stream},
};
use indexmap::IndexSet;
use pin_project::pin_project;
use thiserror::Error;
use tokio::{
    sync::oneshot,
    task::JoinHandle,
    time::{sleep, timeout, Instant},
};
use tower::{Service, ServiceExt};
use tracing::Instrument;

use zebra_chain::{
    block::{self, Block, HeightDiff},
    chain_tip::ChainTip,
    parameters::subsidy::SubsidyError,
};
use zebra_consensus::{error::TransactionError, router::RouterError, BlockError, VerifyBlockError};
use zebra_network::{self as zn, PeerSocketAddr};
use zebra_state as zs;

use crate::components::sync::{BLOCK_DOWNLOAD_TIMEOUT, MIN_CONCURRENCY_LIMIT};

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
/// every such drop. But a peer can also answer a [`BlockDownload`](zn::Request::BlockDownload) request
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
/// A peer can answer a `BlockDownload` request with a canonical header and a rewritten coinbase
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

/// How long to wait for the state to check a block from a peer that negotiated compact block
/// relay against the best tip.
///
/// Timing out keeps the peer's score, so a slow state read can't exempt a block from scoring.
const EARLY_RELAY_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for the parent of a block from a peer that negotiated compact block relay to
/// become the best tip, when the block fails later validation first.
///
/// The peer can relay the block once its parent is the peer's best tip, so the block can fail here
/// before its parent is committed. Timing out keeps the peer's score.
const EARLY_RELAY_PARENT_TIMEOUT: Duration = Duration::from_secs(30);

/// How often to check whether that parent has become the best tip.
const EARLY_RELAY_PARENT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The maximum number of blocks from peers that negotiated compact block relay that the gossiped
/// block downloader remembers as invalid, after they passed the checks that authenticate them for
/// early relay.
///
/// Each of these blocks has valid proof of work at the expected difficulty, as a child of the
/// best tip, so there are only a few of them. The limit bounds their memory on networks where
/// that proof of work is cheap.
const MAX_REJECTED_EARLY_BLOCKS: usize = 100;

/// A block from a peer that negotiated compact block relay failed later validation, after it passed
/// the checks that authenticate it for early relay.
///
/// The draft Compact Block Relay ZIP says that a node must not penalize the peer for this block, so
/// the gossiped block downloader doesn't attribute this error to the peer. The block's hash commits
/// to its transactions and their authorizing data, so the downloader also ignores the hash after
/// this error.
#[derive(Debug, Error)]
#[error("block failed later validation after it was authenticated for early relay: {0}")]
pub struct AuthenticatedBlockRejected(#[source] BoxError);

/// Returns `true` if `error` scores the peer that sent a block, and the block verifier only
/// returns it after the block passed the verifier's part of the checks that authenticate a block
/// for early relay.
///
/// # Security
///
/// The [draft Compact Block Relay ZIP] lets a peer that sets the compact block relay service bit
/// relay a block before it has validated it, once the block passes the ZIP's early relay checks.
/// The ZIP's peer misbehaviour rules say a node must not penalize that peer when such an
/// authenticated block fails later validation.
///
/// Before it verifies a block's transactions, the block verifier checks the block's proof of work,
/// its time against the local clock, its Merkle root, transaction IDs and consensus branch IDs, its
/// coinbase, its shielded action limits, and its subsidy and funding streams when they don't
/// depend on the chain value pools. It only returns these errors once those checks pass:
/// - a transaction failure,
/// - too many transparent signature operations,
/// - a miner fee failure, or
/// - a state commit failure, except authorizing data that doesn't match the block's commitment.
///
/// [`early_block_is_authenticated()`] asks the state for the rest of the early relay checks. Every
/// other error keeps its score, including all the checkpoint verifier's errors.
///
/// [draft Compact Block Relay ZIP]: https://github.com/zcash/zips/pull/1429
pub(crate) fn is_later_validation_failure(
    error: &(dyn std::error::Error + Send + Sync + 'static),
) -> bool {
    let error = match error.downcast_ref::<RouterError>() {
        Some(RouterError::Block { source }) => source.as_ref(),
        // The checkpoint verifier doesn't run the early relay checks.
        Some(RouterError::Checkpoint { .. }) => return false,
        // The inbound block verifier is a router, but its scoring also handles a block verifier
        // error.
        None => match error.downcast_ref::<VerifyBlockError>() {
            Some(error) => error,
            None => return false,
        },
    };

    error.misbehavior_score() != 0
        && match error {
            VerifyBlockError::Transaction(_) => true,
            VerifyBlockError::Block { source } => matches!(
                source,
                BlockError::TooManyTransparentSignatureOperations { .. }
                    // Only the miner fee check returns these subsidy errors as block errors.
                    | BlockError::Transaction(TransactionError::Subsidy(
                        SubsidyError::InvalidMinerFees | SubsidyError::Overflow
                    ))
            ),
            VerifyBlockError::Commit(error) => !error.is_auth_commitment_mismatch(),
            _ => false,
        }
}

/// Returns `true` if the state confirms that `block` passes the rest of the checks that
/// authenticate it for early relay, against the current best tip.
///
/// The state checks that `block` is an NU5 or later child of the best tip, that its difficulty
/// threshold and time are valid in that context, and that its header commits to the best tip's
/// chain history and to the block's authorizing data. A failed or timed-out check returns `false`.
async fn best_tip_commitments_are_valid<ZS>(read_state: ZS, block: Arc<Block>) -> bool
where
    ZS: zs::ReadState,
{
    matches!(
        timeout(
            EARLY_RELAY_CHECK_TIMEOUT,
            read_state.oneshot(zs::ReadRequest::CheckBestTipBlockCommitments(block)),
        )
        .await,
        Ok(Ok(zs::ReadResponse::ValidBestTipBlockCommitments))
    )
}

/// Starts checking `block` against the best tip, if its parent is the best tip, and returns the
/// task that checks it.
///
/// Only call this for a block from a connection that negotiated compact block relay, just before
/// the block is verified. The check runs while the block is verified, so if the block fails later
/// validation, the check doesn't depend on whether another block became the best tip first.
pub(crate) fn start_early_relay_check<ZS, ZSTip>(
    read_state: &ZS,
    latest_chain_tip: &ZSTip,
    block: &Arc<Block>,
) -> Option<JoinHandle<bool>>
where
    ZS: zs::ReadState,
    ZSTip: ChainTip,
{
    (latest_chain_tip.best_tip_hash() == Some(block.header.previous_block_hash)).then(|| {
        tokio::spawn(best_tip_commitments_are_valid(
            read_state.clone(),
            block.clone(),
        ))
    })
}

/// Returns `true` if `block` at `height` passes the rest of the checks that authenticate it for
/// early relay, so a later validation failure doesn't score the peer that sent it.
///
/// `early_check` is the task from [`start_early_relay_check()`], if there is one. Otherwise the
/// state checks the block now.
///
/// Only call this for a block from a connection that negotiated compact block relay, after
/// [`is_later_validation_failure()`] returns `true` for its error.
///
/// # Security
///
/// The peer can relay the block once its parent is the peer's best tip, so the block can fail here
/// before its parent is committed. If the check fails, this waits up to
/// [`EARLY_RELAY_PARENT_TIMEOUT`] for the parent to become the best tip, and checks the block
/// again. If the check fails while the parent is the best tip, it is retried once, in case the
/// best tip changed during the check.
///
/// The peer keeps its score if that check fails too, if a block at the same height or above
/// becomes the best tip instead, or if the parent doesn't become the best tip in time. So a forged
/// body is still scored, and so is a block that can't be checked against the best tip. The
/// expected difficulty of a block depends on its parent's chain, so without that check, the block
/// could have much less proof of work than a valid block.
pub(crate) async fn early_block_is_authenticated<ZS, ZSTip>(
    read_state: ZS,
    latest_chain_tip: ZSTip,
    block: Arc<Block>,
    height: block::Height,
    early_check: Option<JoinHandle<bool>>,
) -> bool
where
    ZS: zs::ReadState,
    ZSTip: ChainTip + Send + 'static,
{
    let authenticated = match early_check {
        // The check task doesn't panic, so it only fails if the runtime is shutting down.
        Some(early_check) => early_check.await.unwrap_or(false),
        None => best_tip_commitments_are_valid(read_state.clone(), block.clone()).await,
    };
    if authenticated {
        return true;
    }

    let parent = block.header.previous_block_hash;
    timeout(EARLY_RELAY_PARENT_TIMEOUT, async move {
        let mut retried = false;
        loop {
            let tip = latest_chain_tip.best_tip_height_and_hash();
            match tip {
                Some((_, tip_hash)) if tip_hash == parent => {
                    // The parent became the best tip, or the last check failed while the parent
                    // was the best tip.
                    if retried {
                        return false;
                    }
                    retried = true;

                    if best_tip_commitments_are_valid(read_state.clone(), block.clone()).await {
                        return true;
                    }
                }
                // The block can only be checked against the best tip, and the best tip has
                // reached its height without it.
                Some((tip_height, _)) if tip_height >= height => return false,
                _ => sleep(EARLY_RELAY_PARENT_POLL_INTERVAL).await,
            }
        }
    })
    .await
    .unwrap_or(false)
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

    /// The block hash belongs to a block from a peer that negotiated compact block relay, which
    /// passed the checks that authenticate it for early relay and then failed later validation,
    /// so this request was ignored.
    ///
    /// The block's hash commits to its transactions and their authorizing data, so the block
    /// can't be valid.
    AlreadyRejected,
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

    /// A read-only state service, which checks a block from a peer that negotiated compact block
    /// relay when the block fails later validation, before the failure is exempt from scoring.
    ///
    /// Without it, no failure is exempt.
    read_state: Option<zs::ReadStateService>,

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

    /// The hashes of blocks from peers that negotiated compact block relay, which passed the
    /// checks that authenticate them for early relay and then failed later validation, oldest
    /// first.
    ///
    /// The peers that sent these blocks aren't scored, so the downloader ignores their hashes
    /// instead of verifying the blocks again. Size-bounded by [`MAX_REJECTED_EARLY_BLOCKS`].
    rejected_early_blocks: IndexSet<block::Hash>,
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
                    Err((e, hash, advertiser_addr)) => {
                        // The block's hash commits to its transactions and their authorizing
                        // data, so a later advertisement of the hash is the same invalid block.
                        if e.is::<AuthenticatedBlockRejected>()
                            && this.rejected_early_blocks.insert(hash)
                            && this.rejected_early_blocks.len() > MAX_REJECTED_EARLY_BLOCKS
                        {
                            this.rejected_early_blocks.shift_remove_index(0);
                        }

                        (Err((e, advertiser_addr)), hash)
                    }
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
    /// The download deadline covers network readiness and the response. Any retry policy
    /// should be applied to `network` without a separate timeout that includes queued requests.
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
            read_state: None,
            latest_chain_tip,
            pending: FuturesUnordered::new(),
            cancel_handles: HashMap::new(),
            in_flight_ips: HashSet::new(),
            rejected_early_blocks: IndexSet::new(),
        }
    }

    /// Uses `read_state` to check the blocks from peers that negotiated compact block relay, so
    /// later validation failures of the blocks that it authenticates don't score those peers.
    ///
    /// See [`is_later_validation_failure()`] and [`early_block_is_authenticated()`].
    pub fn with_read_state(mut self, read_state: zs::ReadStateService) -> Self {
        self.read_state = Some(read_state);
        self
    }

    /// Queue a block for download and verification.
    ///
    /// When `advertiser` is `Some`, it is passed to the network as a known block source,
    /// and its IP is tracked in [`Self::in_flight_ips`] to enforce the one-download-per-IP cap.
    /// `None` bypasses per-IP accounting (for example when Zebra triggers the download internally).
    #[instrument(skip(self, hash), fields(hash = %hash))]
    pub fn download_and_verify(
        &mut self,
        hash: block::Hash,
        advertiser: Option<PeerSocketAddr>,
    ) -> DownloadAction {
        if self.rejected_early_blocks.contains(&hash) {
            debug!(
                ?hash,
                "block hash failed later validation after early relay: ignored block",
            );

            metrics::counter!("gossip.already.rejected.dropped.block.hash.count").increment(1);

            return DownloadAction::AlreadyRejected;
        }

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
        let read_state = self.read_state.clone();
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

            let download_start = Instant::now();
            let response = timeout(
                zn::constants::REQUEST_TIMEOUT + BLOCK_DOWNLOAD_TIMEOUT,
                network.oneshot(zn::Request::BlockDownload { hash, advertiser }),
            )
            .await
            .map_err(|e| (BoxError::from(e), None))?
            .map_err(|e| (e, None))?;
            metrics::histogram!("gossip.block.download.duration_seconds")
                .record(download_start.elapsed().as_secs_f64());
            // A peer that negotiated compact block relay can send a block before it has validated
            // it.
            let early_relay = matches!(response, zn::Response::EarlyBlocks(_));
            let (block, advertiser_addr) = if let zn::Response::Blocks(blocks)
            | zn::Response::EarlyBlocks(blocks) = response
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

            // Keep the block for the state to check, if it fails later validation.
            let early_block = early_relay.then(|| block.clone());
            // Check the block while it is verified, if its parent is the best tip, in case another
            // block becomes the best tip before the verifier rejects this one.
            let early_check = match (&early_block, &read_state) {
                (Some(block), Some(read_state)) => {
                    start_early_relay_check(read_state, &latest_chain_tip, block)
                }
                _ => None,
            };
            let verify_start = Instant::now();
            let result = verifier
                .oneshot(zebra_consensus::Request::Commit(block))
                .await
                .map(|hash| (hash, block_height, verify_start.elapsed()))
                .map_err(|e| (e, advertiser_addr));

            // # Security
            //
            // The draft Compact Block Relay ZIP says that a peer that negotiated compact block
            // relay must not be scored when a block that passes the ZIP's early relay checks fails
            // later validation. The verifier has run its part of those checks before it returns
            // a later validation failure, and the state runs the rest of them. Other blocks and
            // failures keep their score.
            match (result, early_block, read_state) {
                (Err((error, Some(advertiser_addr))), Some(block), Some(read_state))
                    if is_later_validation_failure(&*error) =>
                {
                    if early_block_is_authenticated(
                        read_state,
                        latest_chain_tip,
                        block,
                        block_height,
                        early_check,
                    )
                    .await
                    {
                        debug!(
                            ?hash,
                            ?advertiser_addr,
                            ?error,
                            "authenticated early relay block failed later validation: \
                             rejected it without scoring the peer",
                        );

                        Err((BoxError::from(AuthenticatedBlockRejected(error)), None))
                    } else {
                        Err((error, Some(advertiser_addr)))
                    }
                }
                (result, _, _) => result,
            }
        }
        .map_ok(|(hash, height, verify_commit)| {
            metrics::histogram!("gossip.block.verify_commit.duration_seconds")
                .record(verify_commit.as_secs_f64());
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
