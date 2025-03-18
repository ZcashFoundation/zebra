//! The syncer downloads and verifies large numbers of blocks from peers to Zebra.
//!
//! It is used when Zebra is a long way behind the current chain tip.

use std::{cmp::max, collections::HashSet, convert, pin::Pin, task::Poll, time::Duration};

use color_eyre::eyre::{eyre, Report};
use futures::stream::{FuturesUnordered, StreamExt};
use indexmap::IndexSet;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{mpsc, watch},
    task::JoinError,
    time::{sleep, timeout},
};
use tower::{
    builder::ServiceBuilder, hedge::Hedge, limit::ConcurrencyLimit, retry::Retry, timeout::Timeout,
    Service, ServiceExt,
};

use zebra_chain::{
    block::{self, Height, HeightDiff},
    chain_tip::ChainTip,
};
use zebra_consensus::ParameterCheckpoint as _;
use zebra_network::{self as zn, PeerSocketAddr};
use zebra_state as zs;

use crate::{
    components::sync::downloads::BlockDownloadVerifyError, config::ZebradConfig, BoxError,
};

mod downloads;
pub mod end_of_support;
mod gossip;
mod progress;
mod recent_sync_lengths;
mod status;

#[cfg(test)]
mod tests;

use downloads::{AlwaysHedge, Downloads};

pub use downloads::VERIFICATION_PIPELINE_SCALING_MULTIPLIER;
pub use gossip::{gossip_best_tip_block_hashes, BlockGossipError};
pub use progress::show_block_chain_progress;
pub use recent_sync_lengths::RecentSyncLengths;
pub use status::SyncStatus;

/// Controls the number of peers used for each ObtainTips and ExtendTips request.
const FANOUT: usize = 3;

/// Controls how many times we will retry each block download.
///
/// Failing block downloads is important because it defends against peers who
/// feed us bad hashes. But spurious failures of valid blocks cause the syncer to
/// restart from the previous checkpoint, potentially re-downloading blocks.
///
/// We also hedge requests, so we may retry up to twice this many times. Hedged
/// retries may be concurrent, inner retries are sequential.
const BLOCK_DOWNLOAD_RETRY_LIMIT: usize = 3;

/// A lower bound on the user-specified checkpoint verification concurrency limit.
///
/// Set to the maximum checkpoint interval, so the pipeline holds around a checkpoint's
/// worth of blocks.
///
/// ## Security
///
/// If a malicious node is chosen for an ObtainTips or ExtendTips request, it can
/// provide up to 500 malicious block hashes. These block hashes will be
/// distributed across all available peers. Assuming there are around 50 connected
/// peers, the malicious node will receive approximately 10 of those block requests.
///
/// Malicious deserialized blocks can take up a large amount of RAM, see
/// [`super::inbound::downloads::MAX_INBOUND_CONCURRENCY`] and #1880 for details.
/// So we want to keep the lookahead limit reasonably small.
///
/// Once these malicious blocks start failing validation, the syncer will cancel all
/// the pending download and verify tasks, drop all the blocks, and start a new
/// ObtainTips with a new set of peers.
pub const MIN_CHECKPOINT_CONCURRENCY_LIMIT: usize = zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP;

/// The default for the user-specified lookahead limit.
///
/// See [`MIN_CHECKPOINT_CONCURRENCY_LIMIT`] for details.
pub const DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT: usize = MAX_TIPS_RESPONSE_HASH_COUNT * 2;

/// A lower bound on the user-specified concurrency limit.
///
/// If the concurrency limit is 0, Zebra can't download or verify any blocks.
pub const MIN_CONCURRENCY_LIMIT: usize = 1;

/// The expected maximum number of hashes in an ObtainTips or ExtendTips response.
///
/// This is used to allow block heights that are slightly beyond the lookahead limit,
/// but still limit the number of blocks in the pipeline between the downloader and
/// the state.
///
/// See [`MIN_CHECKPOINT_CONCURRENCY_LIMIT`] for details.
pub const MAX_TIPS_RESPONSE_HASH_COUNT: usize = 500;

/// Controls how long we wait for a tips response to return.
///
/// ## Correctness
///
/// If this timeout is removed (or set too high), the syncer will sometimes hang.
///
/// If this timeout is set too low, the syncer will sometimes get stuck in a
/// failure loop.
pub const TIPS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(6);

/// Controls how long we wait between gossiping successive blocks or transactions.
///
/// ## Correctness
///
/// If this timeout is set too high, blocks and transactions won't propagate through
/// the network efficiently.
///
/// If this timeout is set too low, the peer set and remote peers can get overloaded.
pub const PEER_GOSSIP_DELAY: Duration = Duration::from_secs(7);

/// Controls how long we wait for a block download request to complete.
///
/// This timeout makes sure that the syncer doesn't hang when:
///   - the lookahead queue is full, and
///   - we are waiting for a request that is stuck.
///
/// See [`BLOCK_VERIFY_TIMEOUT`] for details.
///
/// ## Correctness
///
/// If this timeout is removed (or set too high), the syncer will sometimes hang.
///
/// If this timeout is set too low, the syncer will sometimes get stuck in a
/// failure loop.
///
/// We set the timeout so that it requires under 1 Mbps bandwidth for a full 2 MB block.
pub(super) const BLOCK_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(20);

/// Controls how long we wait for a block verify request to complete.
///
/// This timeout makes sure that the syncer doesn't hang when:
///  - the lookahead queue is full, and
///  - all pending verifications:
///    - are waiting on a missing download request,
///    - are waiting on a download or verify request that has failed, but we have
///      deliberately ignored the error,
///    - are for blocks a long way ahead of the current tip, or
///    - are for invalid blocks which will never verify, because they depend on
///      missing blocks or transactions.
///
/// These conditions can happen during normal operation - they are not bugs.
///
/// This timeout also mitigates or hides the following kinds of bugs:
///  - all pending verifications:
///    - are waiting on a download or verify request that has failed, but we have
///      accidentally dropped the error,
///    - are waiting on a download request that has hung inside Zebra,
///    - are on tokio threads that are waiting for blocked operations.
///
/// ## Correctness
///
/// If this timeout is removed (or set too high), the syncer will sometimes hang.
///
/// If this timeout is set too low, the syncer will sometimes get stuck in a
/// failure loop.
///
/// We've observed spurious 15 minute timeouts when a lot of blocks are being committed to
/// the state. But there are also some blocks that seem to hang entirely, and never return.
///
/// So we allow about half the spurious timeout, which might cause some re-downloads.
pub(super) const BLOCK_VERIFY_TIMEOUT: Duration = Duration::from_secs(8 * 60);

/// A shorter timeout used for the first few blocks after the final checkpoint.
///
/// This is a workaround for bug #5125, where the first fully validated blocks
/// after the final checkpoint fail with a timeout, due to a UTXO race condition.
const FINAL_CHECKPOINT_BLOCK_VERIFY_TIMEOUT: Duration = Duration::from_secs(2 * 60);

/// The number of blocks after the final checkpoint that get the shorter timeout.
///
/// We've only seen this error on the first few blocks after the final checkpoint.
const FINAL_CHECKPOINT_BLOCK_VERIFY_TIMEOUT_LIMIT: HeightDiff = 100;

/// Controls how long we wait to restart syncing after finishing a sync run.
///
/// This delay should be long enough to:
///   - allow zcashd peers to process pending requests. If the node only has a
///     few peers, we want to clear as much peer state as possible. In
///     particular, zcashd sends "next block range" hints, based on zcashd's
///     internal model of our sync progress. But we want to discard these hints,
///     so they don't get confused with ObtainTips and ExtendTips responses, and
///   - allow in-progress downloads to time out.
///
/// This delay is particularly important on instances with slow or unreliable
/// networks, and on testnet, which has a small number of slow peers.
///
/// Using a prime number makes sure that syncer fanouts don't synchronise with other crawls.
///
/// ## Correctness
///
/// If this delay is removed (or set too low), the syncer will
/// sometimes get stuck in a failure loop, due to leftover downloads from
/// previous sync runs.
const SYNC_RESTART_DELAY: Duration = Duration::from_secs(67);

/// Controls how long we wait to retry a failed attempt to download
/// and verify the genesis block.
///
/// This timeout gives the crawler time to find better peers.
///
/// ## Security
///
/// If this timeout is removed (or set too low), Zebra will immediately retry
/// to download and verify the genesis block from its peers. This can cause
/// a denial of service on those peers.
///
/// If this timeout is too short, old or buggy nodes will keep making useless
/// network requests. If there are a lot of them, it could overwhelm the network.
const GENESIS_TIMEOUT_RETRY: Duration = Duration::from_secs(10);

/// Sync configuration section.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The number of parallel block download requests.
    ///
    /// This is set to a low value by default, to avoid task and
    /// network contention. Increasing this value may improve
    /// performance on machines with a fast network connection.
    #[serde(alias = "max_concurrent_block_requests")]
    pub download_concurrency_limit: usize,

    /// The number of blocks submitted in parallel to the checkpoint verifier.
    ///
    /// Increasing this limit increases the buffer size, so it reduces
    /// the impact of an individual block request failing. However, it
    /// also increases memory and CPU usage if block validation stalls,
    /// or there are some large blocks in the pipeline.
    ///
    /// The block size limit is 2MB, so in theory, this could represent multiple
    /// gigabytes of data, if we downloaded arbitrary blocks. However,
    /// because we randomly load balance outbound requests, and separate
    /// block download from obtaining block hashes, an adversary would
    /// have to control a significant fraction of our peers to lead us
    /// astray.
    ///
    /// For reliable checkpoint syncing, Zebra enforces a
    /// [`MIN_CHECKPOINT_CONCURRENCY_LIMIT`].
    ///
    /// This is set to a high value by default, to avoid verification pipeline stalls.
    /// Decreasing this value reduces RAM usage.
    #[serde(alias = "lookahead_limit")]
    pub checkpoint_verify_concurrency_limit: usize,

    /// The number of blocks submitted in parallel to the full verifier.
    ///
    /// This is set to a low value by default, to avoid verification timeouts on large blocks.
    /// Increasing this value may improve performance on machines with many cores.
    pub full_verify_concurrency_limit: usize,

    /// The number of threads used to verify signatures, proofs, and other CPU-intensive code.
    ///
    /// If the number of threads is not configured or zero, Zebra uses the number of logical cores.
    /// If the number of logical cores can't be detected, Zebra uses one thread.
    /// For details, see [the `rayon` documentation](https://docs.rs/rayon/latest/rayon/struct.ThreadPoolBuilder.html#method.num_threads).
    pub parallel_cpu_threads: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // 2/3 of the default outbound peer limit.
            download_concurrency_limit: 50,

            // A few max-length checkpoints.
            checkpoint_verify_concurrency_limit: DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT,

            // This default is deliberately very low, so Zebra can verify a few large blocks in under 60 seconds,
            // even on machines with only a few cores.
            //
            // This lets users see the committed block height changing in every progress log,
            // and avoids hangs due to out-of-order verifications flooding the CPUs.
            //
            // TODO:
            // - limit full verification concurrency based on block transaction counts?
            // - move more disk work to blocking tokio threads,
            //   and CPU work to the rayon thread pool inside blocking tokio threads
            full_verify_concurrency_limit: 20,

            // Use one thread per CPU.
            //
            // If this causes tokio executor starvation, move CPU-intensive tasks to rayon threads,
            // or reserve a few cores for tokio threads, based on `num_cpus()`.
            parallel_cpu_threads: 0,
        }
    }
}

/// Helps work around defects in the bitcoin protocol by checking whether
/// the returned hashes actually extend a chain tip.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
struct CheckedTip {
    tip: block::Hash,
    expected_next: block::Hash,
}

pub struct ChainSync<ZN, ZS, ZV, ZSTip>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZS::Future: Send,
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZV::Future: Send,
    ZSTip: ChainTip + Clone + Send + 'static,
{
    // Configuration
    //
    /// The genesis hash for the configured network
    genesis_hash: block::Hash,

    /// The largest block height for the checkpoint verifier, based on the current config.
    max_checkpoint_height: Height,

    /// The configured checkpoint verification concurrency limit, after applying the minimum limit.
    checkpoint_verify_concurrency_limit: usize,

    /// The configured full verification concurrency limit, after applying the minimum limit.
    full_verify_concurrency_limit: usize,

    // Services
    //
    /// A network service which is used to perform ObtainTips and ExtendTips
    /// requests.
    ///
    /// Has no retry logic, because failover is handled using fanout.
    tip_network: Timeout<ZN>,

    /// A service which downloads and verifies blocks, using the provided
    /// network and verifier services.
    downloads: Pin<
        Box<
            Downloads<
                Hedge<ConcurrencyLimit<Retry<zn::RetryLimit, Timeout<ZN>>>, AlwaysHedge>,
                Timeout<ZV>,
                ZSTip,
            >,
        >,
    >,

    /// The cached block chain state.
    state: ZS,

    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: ZSTip,

    // Internal sync state
    //
    /// The tips that the syncer is currently following.
    prospective_tips: HashSet<CheckedTip>,

    /// The lengths of recent sync responses.
    recent_syncs: RecentSyncLengths,

    /// Receiver that is `true` when the downloader is past the lookahead limit.
    /// This is based on the downloaded block height and the state tip height.
    past_lookahead_limit_receiver: zs::WatchReceiver<bool>,

    /// Sender for reporting peer addresses that advertised unexpectedly invalid transactions.
    misbehavior_sender: mpsc::Sender<(PeerSocketAddr, u32)>,
}

/// Polls the network to determine whether further blocks are available and
/// downloads them.
///
/// This component is used for initial block sync, but the `Inbound` service is
/// responsible for participating in the gossip protocols used for block
/// diffusion.
impl<ZN, ZS, ZV, ZSTip> ChainSync<ZN, ZS, ZV, ZSTip>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZS::Future: Send,
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZV::Future: Send,
    ZSTip: ChainTip + Clone + Send + 'static,
{
    /// Returns a new syncer instance, using:
    ///  - chain: the zebra-chain `Network` to download (Mainnet or Testnet)
    ///  - peers: the zebra-network peers to contact for downloads
    ///  - verifier: the zebra-consensus verifier that checks the chain
    ///  - state: the zebra-state that stores the chain
    ///  - latest_chain_tip: the latest chain tip from `state`
    ///
    /// Also returns a [`SyncStatus`] to check if the syncer has likely reached the chain tip.
    pub fn new(
        config: &ZebradConfig,
        max_checkpoint_height: Height,
        peers: ZN,
        verifier: ZV,
        state: ZS,
        latest_chain_tip: ZSTip,
        misbehavior_sender: mpsc::Sender<(PeerSocketAddr, u32)>,
    ) -> (Self, SyncStatus) {
        let mut download_concurrency_limit = config.sync.download_concurrency_limit;
        let mut checkpoint_verify_concurrency_limit =
            config.sync.checkpoint_verify_concurrency_limit;
        let mut full_verify_concurrency_limit = config.sync.full_verify_concurrency_limit;

        if download_concurrency_limit < MIN_CONCURRENCY_LIMIT {
            warn!(
                "configured download concurrency limit {} too low, increasing to {}",
                config.sync.download_concurrency_limit, MIN_CONCURRENCY_LIMIT,
            );

            download_concurrency_limit = MIN_CONCURRENCY_LIMIT;
        }

        if checkpoint_verify_concurrency_limit < MIN_CHECKPOINT_CONCURRENCY_LIMIT {
            warn!(
                "configured checkpoint verify concurrency limit {} too low, increasing to {}",
                config.sync.checkpoint_verify_concurrency_limit, MIN_CHECKPOINT_CONCURRENCY_LIMIT,
            );

            checkpoint_verify_concurrency_limit = MIN_CHECKPOINT_CONCURRENCY_LIMIT;
        }

        if full_verify_concurrency_limit < MIN_CONCURRENCY_LIMIT {
            warn!(
                "configured full verify concurrency limit {} too low, increasing to {}",
                config.sync.full_verify_concurrency_limit, MIN_CONCURRENCY_LIMIT,
            );

            full_verify_concurrency_limit = MIN_CONCURRENCY_LIMIT;
        }

        let tip_network = Timeout::new(peers.clone(), TIPS_RESPONSE_TIMEOUT);

        // The Hedge middleware is the outermost layer, hedging requests
        // between two retry-wrapped networks.  The innermost timeout
        // layer is relatively unimportant, because slow requests will
        // probably be preemptively hedged.
        //
        // The Hedge goes outside the Retry, because the Retry layer
        // abstracts away spurious failures from individual peers
        // making a less-fallible network service, and the Hedge layer
        // tries to reduce latency of that less-fallible service.
        let block_network = Hedge::new(
            ServiceBuilder::new()
                .concurrency_limit(download_concurrency_limit)
                .retry(zn::RetryLimit::new(BLOCK_DOWNLOAD_RETRY_LIMIT))
                .timeout(BLOCK_DOWNLOAD_TIMEOUT)
                .service(peers),
            AlwaysHedge,
            20,
            0.95,
            2 * SYNC_RESTART_DELAY,
        );

        // We apply a timeout to the verifier to avoid hangs due to missing earlier blocks.
        let verifier = Timeout::new(verifier, BLOCK_VERIFY_TIMEOUT);

        let (sync_status, recent_syncs) = SyncStatus::new();

        let (past_lookahead_limit_sender, past_lookahead_limit_receiver) = watch::channel(false);
        let past_lookahead_limit_receiver = zs::WatchReceiver::new(past_lookahead_limit_receiver);

        let downloads = Box::pin(Downloads::new(
            block_network,
            verifier,
            latest_chain_tip.clone(),
            past_lookahead_limit_sender,
            max(
                checkpoint_verify_concurrency_limit,
                full_verify_concurrency_limit,
            ),
            max_checkpoint_height,
        ));

        let new_syncer = Self {
            genesis_hash: config.network.network.genesis_hash(),
            max_checkpoint_height,
            checkpoint_verify_concurrency_limit,
            full_verify_concurrency_limit,
            tip_network,
            downloads,
            state,
            latest_chain_tip,
            prospective_tips: HashSet::new(),
            recent_syncs,
            past_lookahead_limit_receiver,
            misbehavior_sender,
        };

        (new_syncer, sync_status)
    }

    /// Runs the syncer to synchronize the chain and keep it synchronized.
    #[instrument(skip(self))]
    pub async fn sync(mut self) -> Result<(), Report> {
        // We can't download the genesis block using our normal algorithm,
        // due to protocol limitations
        self.request_genesis().await?;

        loop {
            if self.try_to_sync().await.is_err() {
                self.downloads.cancel_all();
            }

            self.update_metrics();

            info!(
                timeout = ?SYNC_RESTART_DELAY,
                state_tip = ?self.latest_chain_tip.best_tip_height(),
                "waiting to restart sync"
            );
            sleep(SYNC_RESTART_DELAY).await;
        }
    }

    /// Tries to synchronize the chain as far as it can.
    ///
    /// Obtains some prospective tips and iteratively tries to extend them and download the missing
    /// blocks.
    ///
    /// Returns `Ok` if it was able to synchronize as much of the chain as it could, and then ran
    /// out of prospective tips. This happens when synchronization finishes or if Zebra ended up
    /// following a fork. Either way, Zebra should attempt to obtain some more tips.
    ///
    /// Returns `Err` if there was an unrecoverable error and restarting the synchronization is
    /// necessary. This includes outer timeouts, where an entire syncing step takes an extremely
    /// long time. (These usually indicate hangs.)
    #[instrument(skip(self))]
    async fn try_to_sync(&mut self) -> Result<(), Report> {
        self.prospective_tips = HashSet::new();

        info!(
            state_tip = ?self.latest_chain_tip.best_tip_height(),
            "starting sync, obtaining new tips"
        );
        let mut extra_hashes = timeout(SYNC_RESTART_DELAY, self.obtain_tips())
            .await
            .map_err(Into::into)
            // TODO: replace with flatten() when it stabilises (#70142)
            .and_then(convert::identity)
            .map_err(|e| {
                info!("temporary error obtaining tips: {:#}", e);
                e
            })?;
        self.update_metrics();

        while !self.prospective_tips.is_empty() || !extra_hashes.is_empty() {
            // Avoid hangs due to service readiness or other internal operations
            extra_hashes = timeout(BLOCK_VERIFY_TIMEOUT, self.try_to_sync_once(extra_hashes))
                .await
                .map_err(Into::into)
                // TODO: replace with flatten() when it stabilises (#70142)
                .and_then(convert::identity)?;
        }

        info!("exhausted prospective tip set");

        Ok(())
    }

    /// Tries to synchronize the chain once, using the existing `extra_hashes`.
    ///
    /// Tries to extend the existing tips and download the missing blocks.
    ///
    /// Returns `Ok(extra_hashes)` if it was able to extend once and synchronize sone of the chain.
    /// Returns `Err` if there was an unrecoverable error and restarting the synchronization is
    /// necessary.
    #[instrument(skip(self))]
    async fn try_to_sync_once(
        &mut self,
        mut extra_hashes: IndexSet<block::Hash>,
    ) -> Result<IndexSet<block::Hash>, Report> {
        // Check whether any block tasks are currently ready.
        while let Poll::Ready(Some(rsp)) = futures::poll!(self.downloads.next()) {
            // Some temporary errors are ignored, and syncing continues with other blocks.
            // If it turns out they were actually important, syncing will run out of blocks, and
            // the syncer will reset itself.
            self.handle_block_response(rsp)?;
        }
        self.update_metrics();

        // Pause new downloads while the syncer or downloader are past their lookahead limits.
        //
        // To avoid a deadlock or long waits for blocks to expire, we ignore the download
        // lookahead limit when there are only a small number of blocks waiting.
        while self.downloads.in_flight() >= self.lookahead_limit(extra_hashes.len())
            || (self.downloads.in_flight() >= self.lookahead_limit(extra_hashes.len()) / 2
                && self.past_lookahead_limit_receiver.cloned_watch_data())
        {
            trace!(
                tips.len = self.prospective_tips.len(),
                in_flight = self.downloads.in_flight(),
                extra_hashes = extra_hashes.len(),
                lookahead_limit = self.lookahead_limit(extra_hashes.len()),
                state_tip = ?self.latest_chain_tip.best_tip_height(),
                "waiting for pending blocks",
            );

            let response = self.downloads.next().await.expect("downloads is nonempty");

            self.handle_block_response(response)?;
            self.update_metrics();
        }

        // Once we're below the lookahead limit, we can request more blocks or hashes.
        if !extra_hashes.is_empty() {
            debug!(
                tips.len = self.prospective_tips.len(),
                in_flight = self.downloads.in_flight(),
                extra_hashes = extra_hashes.len(),
                lookahead_limit = self.lookahead_limit(extra_hashes.len()),
                state_tip = ?self.latest_chain_tip.best_tip_height(),
                "requesting more blocks",
            );

            let response = self.request_blocks(extra_hashes).await;
            extra_hashes = Self::handle_hash_response(response)?;
        } else {
            info!(
                tips.len = self.prospective_tips.len(),
                in_flight = self.downloads.in_flight(),
                extra_hashes = extra_hashes.len(),
                lookahead_limit = self.lookahead_limit(extra_hashes.len()),
                state_tip = ?self.latest_chain_tip.best_tip_height(),
                "extending tips",
            );

            extra_hashes = self.extend_tips().await.map_err(|e| {
                info!("temporary error extending tips: {:#}", e);
                e
            })?;
        }
        self.update_metrics();

        Ok(extra_hashes)
    }

    /// Given a block_locator list fan out request for subsequent hashes to
    /// multiple peers
    #[instrument(skip(self))]
    async fn obtain_tips(&mut self) -> Result<IndexSet<block::Hash>, Report> {
        let block_locator = self
            .state
            .ready()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::BlockLocator)
            .await
            .map(|response| match response {
                zebra_state::Response::BlockLocator(block_locator) => block_locator,
                _ => unreachable!(
                    "GetBlockLocator request can only result in Response::BlockLocator"
                ),
            })
            .map_err(|e| eyre!(e))?;

        debug!(
            tip = ?block_locator.first().expect("we have at least one block locator object"),
            ?block_locator,
            "got block locator and trying to obtain new chain tips"
        );

        let mut requests = FuturesUnordered::new();
        for attempt in 0..FANOUT {
            if attempt > 0 {
                // Let other tasks run, so we're more likely to choose a different peer.
                //
                // TODO: move fanouts into the PeerSet, so we always choose different peers (#2214)
                tokio::task::yield_now().await;
            }

            let ready_tip_network = self.tip_network.ready().await;
            requests.push(tokio::spawn(ready_tip_network.map_err(|e| eyre!(e))?.call(
                zn::Request::FindBlocks {
                    known_blocks: block_locator.clone(),
                    stop: None,
                },
            )));
        }

        let mut download_set = IndexSet::new();
        while let Some(res) = requests.next().await {
            match res
                .unwrap_or_else(|e @ JoinError { .. }| {
                    if e.is_panic() {
                        panic!("panic in obtain tips task: {e:?}");
                    } else {
                        info!(
                            "task error during obtain tips task: {e:?},\
                     is Zebra shutting down?"
                        );
                        Err(e.into())
                    }
                })
                .map_err::<Report, _>(|e| eyre!(e))
            {
                Ok(zn::Response::BlockHashes(hashes)) => {
                    trace!(?hashes);

                    // zcashd sometimes appends an unrelated hash at the start
                    // or end of its response.
                    //
                    // We can't discard the first hash, because it might be a
                    // block we want to download. So we just accept any
                    // out-of-order first hashes.

                    // We use the last hash for the tip, and we want to avoid bad
                    // tips. So we discard the last hash. (We don't need to worry
                    // about missed downloads, because we will pick them up again
                    // in ExtendTips.)
                    let hashes = match hashes.as_slice() {
                        [] => continue,
                        [rest @ .., _last] => rest,
                    };

                    let mut first_unknown = None;
                    for (i, &hash) in hashes.iter().enumerate() {
                        if !self.state_contains(hash).await? {
                            first_unknown = Some(i);
                            break;
                        }
                    }

                    debug!(hashes.len = ?hashes.len(), ?first_unknown);

                    let unknown_hashes = if let Some(index) = first_unknown {
                        &hashes[index..]
                    } else {
                        continue;
                    };

                    trace!(?unknown_hashes);

                    let new_tip = if let Some(end) = unknown_hashes.rchunks_exact(2).next() {
                        CheckedTip {
                            tip: end[0],
                            expected_next: end[1],
                        }
                    } else {
                        debug!("discarding response that extends only one block");
                        continue;
                    };

                    // Make sure we get the same tips, regardless of the
                    // order of peer responses
                    if !download_set.contains(&new_tip.expected_next) {
                        debug!(?new_tip,
                                        "adding new prospective tip, and removing existing tips in the new block hash list");
                        self.prospective_tips
                            .retain(|t| !unknown_hashes.contains(&t.expected_next));
                        self.prospective_tips.insert(new_tip);
                    } else {
                        debug!(
                            ?new_tip,
                            "discarding prospective tip: already in download set"
                        );
                    }

                    // security: the first response determines our download order
                    //
                    // TODO: can we make the download order independent of response order?
                    let prev_download_len = download_set.len();
                    download_set.extend(unknown_hashes);
                    let new_download_len = download_set.len();
                    let new_hashes = new_download_len - prev_download_len;
                    debug!(new_hashes, "added hashes to download set");
                    metrics::histogram!("sync.obtain.response.hash.count")
                        .record(new_hashes as f64);
                }
                Ok(_) => unreachable!("network returned wrong response"),
                // We ignore this error because we made multiple fanout requests.
                Err(e) => debug!(?e),
            }
        }

        debug!(?self.prospective_tips);

        // Check that the new tips we got are actually unknown.
        for hash in &download_set {
            debug!(?hash, "checking if state contains hash");
            if self.state_contains(*hash).await? {
                return Err(eyre!("queued download of hash behind our chain tip"));
            }
        }

        let new_downloads = download_set.len();
        debug!(new_downloads, "queueing new downloads");
        metrics::gauge!("sync.obtain.queued.hash.count").set(new_downloads as f64);

        // security: use the actual number of new downloads from all peers,
        // so the last peer to respond can't toggle our mempool
        self.recent_syncs.push_obtain_tips_length(new_downloads);

        let response = self.request_blocks(download_set).await;

        Self::handle_hash_response(response).map_err(Into::into)
    }

    #[instrument(skip(self))]
    async fn extend_tips(&mut self) -> Result<IndexSet<block::Hash>, Report> {
        let tips = std::mem::take(&mut self.prospective_tips);

        let mut download_set = IndexSet::new();
        debug!(tips = ?tips.len(), "trying to extend chain tips");
        for tip in tips {
            debug!(?tip, "asking peers to extend chain tip");
            let mut responses = FuturesUnordered::new();
            for attempt in 0..FANOUT {
                if attempt > 0 {
                    // Let other tasks run, so we're more likely to choose a different peer.
                    //
                    // TODO: move fanouts into the PeerSet, so we always choose different peers (#2214)
                    tokio::task::yield_now().await;
                }

                let ready_tip_network = self.tip_network.ready().await;
                responses.push(tokio::spawn(ready_tip_network.map_err(|e| eyre!(e))?.call(
                    zn::Request::FindBlocks {
                        known_blocks: vec![tip.tip],
                        stop: None,
                    },
                )));
            }
            while let Some(res) = responses.next().await {
                match res
                    .expect("panic in spawned extend tips request")
                    .map_err::<Report, _>(|e| eyre!(e))
                {
                    Ok(zn::Response::BlockHashes(hashes)) => {
                        debug!(first = ?hashes.first(), len = ?hashes.len());
                        trace!(?hashes);

                        // zcashd sometimes appends an unrelated hash at the
                        // start or end of its response. Check the first hash
                        // against the previous response, and discard mismatches.
                        let unknown_hashes = match hashes.as_slice() {
                            [expected_hash, rest @ ..] if expected_hash == &tip.expected_next => {
                                rest
                            }
                            // If the first hash doesn't match, retry with the second.
                            [first_hash, expected_hash, rest @ ..]
                                if expected_hash == &tip.expected_next =>
                            {
                                debug!(?first_hash,
                                                ?tip.expected_next,
                                                ?tip.tip,
                                                "unexpected first hash, but the second matches: using the hashes after the match");
                                rest
                            }
                            // We ignore these responses
                            [] => continue,
                            [single_hash] => {
                                debug!(?single_hash,
                                                ?tip.expected_next,
                                                ?tip.tip,
                                                "discarding response containing a single unexpected hash");
                                continue;
                            }
                            [first_hash, second_hash, rest @ ..] => {
                                debug!(?first_hash,
                                                ?second_hash,
                                                rest_len = ?rest.len(),
                                                ?tip.expected_next,
                                                ?tip.tip,
                                                "discarding response that starts with two unexpected hashes");
                                continue;
                            }
                        };

                        // We use the last hash for the tip, and we want to avoid
                        // bad tips. So we discard the last hash. (We don't need
                        // to worry about missed downloads, because we will pick
                        // them up again in the next ExtendTips.)
                        let unknown_hashes = match unknown_hashes {
                            [] => continue,
                            [rest @ .., _last] => rest,
                        };

                        let new_tip = if let Some(end) = unknown_hashes.rchunks_exact(2).next() {
                            CheckedTip {
                                tip: end[0],
                                expected_next: end[1],
                            }
                        } else {
                            debug!("discarding response that extends only one block");
                            continue;
                        };

                        trace!(?unknown_hashes);

                        // Make sure we get the same tips, regardless of the
                        // order of peer responses
                        if !download_set.contains(&new_tip.expected_next) {
                            debug!(?new_tip,
                                            "adding new prospective tip, and removing any existing tips in the new block hash list");
                            self.prospective_tips
                                .retain(|t| !unknown_hashes.contains(&t.expected_next));
                            self.prospective_tips.insert(new_tip);
                        } else {
                            debug!(
                                ?new_tip,
                                "discarding prospective tip: already in download set"
                            );
                        }

                        // security: the first response determines our download order
                        //
                        // TODO: can we make the download order independent of response order?
                        let prev_download_len = download_set.len();
                        download_set.extend(unknown_hashes);
                        let new_download_len = download_set.len();
                        let new_hashes = new_download_len - prev_download_len;
                        debug!(new_hashes, "added hashes to download set");
                        metrics::histogram!("sync.extend.response.hash.count")
                            .record(new_hashes as f64);
                    }
                    Ok(_) => unreachable!("network returned wrong response"),
                    // We ignore this error because we made multiple fanout requests.
                    Err(e) => debug!(?e),
                }
            }
        }

        let new_downloads = download_set.len();
        debug!(new_downloads, "queueing new downloads");
        metrics::gauge!("sync.extend.queued.hash.count").set(new_downloads as f64);

        // security: use the actual number of new downloads from all peers,
        // so the last peer to respond can't toggle our mempool
        self.recent_syncs.push_extend_tips_length(new_downloads);

        let response = self.request_blocks(download_set).await;

        Self::handle_hash_response(response).map_err(Into::into)
    }

    /// Download and verify the genesis block, if it isn't currently known to
    /// our node.
    async fn request_genesis(&mut self) -> Result<(), Report> {
        // Due to Bitcoin protocol limitations, we can't request the genesis
        // block using our standard tip-following algorithm:
        //  - getblocks requires at least one hash
        //  - responses start with the block *after* the requested block, and
        //  - the genesis hash is used as a placeholder for "no matches".
        //
        // So we just download and verify the genesis block here.
        while !self.state_contains(self.genesis_hash).await? {
            info!("starting genesis block download and verify");

            let response = timeout(SYNC_RESTART_DELAY, self.request_genesis_once())
                .await
                .map_err(Into::into);

            // 3 layers of results is not ideal, but we need the timeout on the outside.
            match response {
                Ok(Ok(Ok(response))) => self
                    .handle_block_response(Ok(response))
                    .expect("never returns Err for Ok"),
                // Handle fatal errors
                Ok(Err(fatal_error)) => Err(fatal_error)?,
                // Handle timeouts and block errors
                Err(error) | Ok(Ok(Err(error))) => {
                    // TODO: exit syncer on permanent service errors (NetworkError, VerifierError)
                    if Self::should_restart_sync(&error) {
                        warn!(
                            ?error,
                            "could not download or verify genesis block, retrying"
                        );
                    } else {
                        info!(
                            ?error,
                            "temporary error downloading or verifying genesis block, retrying"
                        );
                    }

                    tokio::time::sleep(GENESIS_TIMEOUT_RETRY).await;
                }
            }
        }

        Ok(())
    }

    /// Try to download and verify the genesis block once.
    ///
    /// Fatal errors are returned in the outer result, temporary errors in the inner one.
    async fn request_genesis_once(
        &mut self,
    ) -> Result<Result<(Height, block::Hash), BlockDownloadVerifyError>, Report> {
        let response = self.downloads.download_and_verify(self.genesis_hash).await;
        Self::handle_response(response).map_err(|e| eyre!(e))?;

        let response = self.downloads.next().await.expect("downloads is nonempty");

        Ok(response)
    }

    /// Queue download and verify tasks for each block that isn't currently known to our node.
    ///
    /// TODO: turn obtain and extend tips into a separate task, which sends hashes via a channel?
    async fn request_blocks(
        &mut self,
        mut hashes: IndexSet<block::Hash>,
    ) -> Result<IndexSet<block::Hash>, BlockDownloadVerifyError> {
        let lookahead_limit = self.lookahead_limit(hashes.len());

        debug!(
            hashes.len = hashes.len(),
            ?lookahead_limit,
            "requesting blocks",
        );

        let extra_hashes = if hashes.len() > lookahead_limit {
            hashes.split_off(lookahead_limit)
        } else {
            IndexSet::new()
        };

        for hash in hashes.into_iter() {
            self.downloads.download_and_verify(hash).await?;
        }

        Ok(extra_hashes)
    }

    /// The configured lookahead limit, based on the currently verified height,
    /// and the number of hashes we haven't queued yet.
    fn lookahead_limit(&self, new_hashes: usize) -> usize {
        let max_checkpoint_height: usize = self
            .max_checkpoint_height
            .0
            .try_into()
            .expect("fits in usize");

        // When the state is empty, we want to verify using checkpoints
        let verified_height: usize = self
            .latest_chain_tip
            .best_tip_height()
            .unwrap_or(Height(0))
            .0
            .try_into()
            .expect("fits in usize");

        if verified_height >= max_checkpoint_height {
            self.full_verify_concurrency_limit
        } else if (verified_height + new_hashes) >= max_checkpoint_height {
            // If we're just about to start full verification, allow enough for the remaining checkpoint,
            // and also enough for a separate full verification lookahead.
            let checkpoint_hashes = verified_height + new_hashes - max_checkpoint_height;

            self.full_verify_concurrency_limit + checkpoint_hashes
        } else {
            self.checkpoint_verify_concurrency_limit
        }
    }

    /// Handles a response for a requested block.
    ///
    /// See [`Self::handle_response`] for more details.
    #[allow(unknown_lints)]
    fn handle_block_response(
        &mut self,
        response: Result<(Height, block::Hash), BlockDownloadVerifyError>,
    ) -> Result<(), BlockDownloadVerifyError> {
        match response {
            Ok((height, hash)) => {
                trace!(?height, ?hash, "verified and committed block to state");

                return Ok(());
            }

            Err(BlockDownloadVerifyError::Invalid {
                ref error,
                advertiser_addr: Some(advertiser_addr),
                ..
            }) if error.misbehavior_score() != 0 => {
                let _ = self
                    .misbehavior_sender
                    .try_send((advertiser_addr, error.misbehavior_score()));
            }

            Err(_) => {}
        };

        Self::handle_response(response)
    }

    /// Handles a response to block hash submission, passing through any extra hashes.
    ///
    /// See [`Self::handle_response`] for more details.
    #[allow(unknown_lints)]
    fn handle_hash_response(
        response: Result<IndexSet<block::Hash>, BlockDownloadVerifyError>,
    ) -> Result<IndexSet<block::Hash>, BlockDownloadVerifyError> {
        match response {
            Ok(extra_hashes) => Ok(extra_hashes),
            Err(_) => Self::handle_response(response).map(|()| IndexSet::new()),
        }
    }

    /// Handles a response to a syncer request.
    ///
    /// Returns `Ok` if the request was successful, or if an expected error occurred,
    /// so that the synchronization can continue normally.
    ///
    /// Returns `Err` if an unexpected error occurred, to force the synchronizer to restart.
    #[allow(unknown_lints)]
    fn handle_response<T>(
        response: Result<T, BlockDownloadVerifyError>,
    ) -> Result<(), BlockDownloadVerifyError> {
        match response {
            Ok(_t) => Ok(()),
            Err(error) => {
                // TODO: exit syncer on permanent service errors (NetworkError, VerifierError)
                if Self::should_restart_sync(&error) {
                    Err(error)
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Returns `true` if the hash is present in the state, and `false`
    /// if the hash is not present in the state.
    pub(crate) async fn state_contains(&mut self, hash: block::Hash) -> Result<bool, Report> {
        match self
            .state
            .ready()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::KnownBlock(hash))
            .await
            .map_err(|e| eyre!(e))?
        {
            zs::Response::KnownBlock(loc) => Ok(loc.is_some()),
            _ => unreachable!("wrong response to known block request"),
        }
    }

    fn update_metrics(&mut self) {
        metrics::gauge!("sync.prospective_tips.len",).set(self.prospective_tips.len() as f64);
        metrics::gauge!("sync.downloads.in_flight",).set(self.downloads.in_flight() as f64);
    }

    /// Return if the sync should be restarted based on the given error
    /// from the block downloader and verifier stream.
    fn should_restart_sync(e: &BlockDownloadVerifyError) -> bool {
        match e {
            // Structural matches: downcasts
            BlockDownloadVerifyError::Invalid { error, .. } if error.is_duplicate_request() => {
                debug!(error = ?e, "block was already verified, possibly from a previous sync run, continuing");
                false
            }

            // Structural matches: direct
            BlockDownloadVerifyError::CancelledDuringDownload { .. }
            | BlockDownloadVerifyError::CancelledDuringVerification { .. } => {
                debug!(error = ?e, "block verification was cancelled, continuing");
                false
            }
            BlockDownloadVerifyError::BehindTipHeightLimit { .. } => {
                debug!(
                    error = ?e,
                    "block height is behind the current state tip, \
                     assuming the syncer will eventually catch up to the state, continuing"
                );
                false
            }
            BlockDownloadVerifyError::DuplicateBlockQueuedForDownload { .. } => {
                debug!(
                    error = ?e,
                    "queued duplicate block hash for download, \
                     assuming the syncer will eventually resolve duplicates, continuing"
                );
                false
            }

            // String matches
            //
            // We want to match VerifyChainError::Block(VerifyBlockError::Commit(ref source)),
            // but that type is boxed.
            // TODO:
            // - turn this check into a function on VerifyChainError, like is_duplicate_request()
            BlockDownloadVerifyError::Invalid { error, .. }
                if format!("{error:?}").contains("block is already committed to the state")
                    || format!("{error:?}")
                        .contains("block has already been sent to be committed to the state") =>
            {
                // TODO: improve this by checking the type (#2908)
                debug!(error = ?e, "block is already committed or pending a commit, possibly from a previous sync run, continuing");
                false
            }
            BlockDownloadVerifyError::DownloadFailed { ref error, .. }
                if format!("{error:?}").contains("NotFound") =>
            {
                // Covers these errors:
                // - NotFoundResponse
                // - NotFoundRegistry
                //
                // TODO: improve this by checking the type (#2908)
                //       restart after a certain number of NotFound errors?
                debug!(error = ?e, "block was not found, possibly from a peer that doesn't have the block yet, continuing");
                false
            }

            _ => {
                // download_and_verify downcasts errors from the block verifier
                // into VerifyChainError, and puts the result inside one of the
                // BlockDownloadVerifyError enumerations. This downcast could
                // become incorrect e.g. after some refactoring, and it is difficult
                // to write a test to check it. The test below is a best-effort
                // attempt to catch if that happens and log it.
                //
                // TODO: add a proper test and remove this
                // https://github.com/ZcashFoundation/zebra/issues/2909
                let err_str = format!("{e:?}");
                if err_str.contains("AlreadyVerified")
                    || err_str.contains("AlreadyInChain")
                    || err_str.contains("block is already committed to the state")
                    || err_str.contains("block has already been sent to be committed to the state")
                    || err_str.contains("NotFound")
                {
                    error!(?e,
                        "a BlockDownloadVerifyError that should have been filtered out was detected, \
                        which possibly indicates a programming error in the downcast inside \
                        zebrad::components::sync::downloads::Downloads::download_and_verify"
                    )
                }

                warn!(?e, "error downloading and verifying block");
                true
            }
        }
    }
}
