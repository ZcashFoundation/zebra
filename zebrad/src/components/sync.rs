use std::{collections::HashSet, pin::Pin, sync::Arc, time::Duration};

use color_eyre::eyre::{eyre, Report};
use futures::{
    future::FutureExt,
    stream::{FuturesUnordered, StreamExt},
};
use tokio::{sync::watch, time::sleep};
use tower::{
    builder::ServiceBuilder, hedge::Hedge, limit::ConcurrencyLimit, retry::Retry, timeout::Timeout,
    Service, ServiceExt,
};

use zebra_chain::{
    block::{self, Block},
    parameters::genesis_hash,
};
use zebra_network as zn;
use zebra_state as zs;

use crate::{config::ZebradConfig, BoxError};

mod downloads;
mod recent_sync_lengths;

#[cfg(test)]
mod tests;

use downloads::{AlwaysHedge, Downloads};
use recent_sync_lengths::RecentSyncLengths;

/// Controls the number of peers used for each ObtainTips and ExtendTips request.
const FANOUT: usize = 4;

/// Controls how many times we will retry each block download.
///
/// Failing block downloads is important because it defends against peers who
/// feed us bad hashes. But spurious failures of valid blocks cause the syncer to
/// restart from the previous checkpoint, potentially re-downloading blocks.
///
/// We also hedge requests, so we may retry up to twice this many times. Hedged
/// retries may be concurrent, inner retries are sequential.
const BLOCK_DOWNLOAD_RETRY_LIMIT: usize = 2;

/// A lower bound on the user-specified lookahead limit.
///
/// Set to two checkpoint intervals, so that we're sure that the lookahead
/// limit always contains at least one complete checkpoint.
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
const MIN_LOOKAHEAD_LIMIT: usize = zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP * 2;

/// Controls how long we wait for a tips response to return.
///
/// ## Correctness
///
/// If this timeout is removed (or set too high), the syncer will sometimes hang.
///
/// If this timeout is set too low, the syncer will sometimes get stuck in a
/// failure loop.
const TIPS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(6);

/// Controls how long we wait for a block download request to complete.
///
/// This timeout makes sure that the syncer doesn't hang when:
///   - the lookahead queue is full, and
///   - we are waiting for a request that is stuck.
/// See [`BLOCK_VERIFY_TIMEOUT`] for details.
///
/// ## Correctness
///
/// If this timeout is removed (or set too high), the syncer will sometimes hang.
///
/// If this timeout is set too low, the syncer will sometimes get stuck in a
/// failure loop.
pub(super) const BLOCK_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15);

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
pub(super) const BLOCK_VERIFY_TIMEOUT: Duration = Duration::from_secs(180);

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
/// ## Correctness
///
/// If this delay is removed (or set too low), the syncer will
/// sometimes get stuck in a failure loop, due to leftover downloads from
/// previous sync runs.
const SYNC_RESTART_DELAY: Duration = Duration::from_secs(61);

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
const GENESIS_TIMEOUT_RETRY: Duration = Duration::from_secs(5);

/// Helps work around defects in the bitcoin protocol by checking whether
/// the returned hashes actually extend a chain tip.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
struct CheckedTip {
    tip: block::Hash,
    expected_next: block::Hash,
}

pub struct ChainSync<ZN, ZS, ZV>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
{
    // Configuration
    /// The genesis hash for the configured network
    genesis_hash: block::Hash,

    /// The configured lookahead limit, after applying the minimum limit.
    lookahead_limit: usize,

    // Services
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
            >,
        >,
    >,

    /// The cached block chain state.
    state: ZS,

    // Internal sync state
    /// The tips that the syncer is currently following.
    prospective_tips: HashSet<CheckedTip>,

    /// The lengths of recent sync responses.
    recent_syncs: RecentSyncLengths,
}

/// Polls the network to determine whether further blocks are available and
/// downloads them.
///
/// This component is used for initial block sync, but the `Inbound` service is
/// responsible for participating in the gossip protocols used for block
/// diffusion.
impl<ZN, ZS, ZV> ChainSync<ZN, ZS, ZV>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = BoxError> + Send + Clone + 'static,
    ZV::Future: Send,
{
    /// Returns a new syncer instance, using:
    ///  - chain: the zebra-chain `Network` to download (Mainnet or Testnet)
    ///  - peers: the zebra-network peers to contact for downloads
    ///  - state: the zebra-state that stores the chain
    ///  - verifier: the zebra-consensus verifier that checks the chain
    ///
    /// Also returns a [`watch::Receiver`] endpoint for receiving recent sync lengths.
    pub fn new(
        config: &ZebradConfig,
        peers: ZN,
        state: ZS,
        verifier: ZV,
    ) -> (Self, watch::Receiver<Vec<usize>>) {
        let tip_network = Timeout::new(peers.clone(), TIPS_RESPONSE_TIMEOUT);
        // The Hedge middleware is the outermost layer, hedging requests
        // between two retry-wrapped networks.  The innermost timeout
        // layer is relatively unimportant, because slow requests will
        // probably be pre-emptively hedged.
        //
        // The Hedge goes outside the Retry, because the Retry layer
        // abstracts away spurious failures from individual peers
        // making a less-fallible network service, and the Hedge layer
        // tries to reduce latency of that less-fallible service.
        //
        // XXX add ServiceBuilder::hedge() so this becomes
        // ServiceBuilder::new().hedge(...).retry(...)...
        let block_network = Hedge::new(
            ServiceBuilder::new()
                .concurrency_limit(config.sync.max_concurrent_block_requests)
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

        assert!(
            config.sync.lookahead_limit >= MIN_LOOKAHEAD_LIMIT,
            "configured lookahead limit {} too low, must be at least {}",
            config.sync.lookahead_limit,
            MIN_LOOKAHEAD_LIMIT
        );

        let (recent_syncs, sync_length_receiver) = RecentSyncLengths::new();

        let new_syncer = Self {
            genesis_hash: genesis_hash(config.network.network),
            lookahead_limit: config.sync.lookahead_limit,
            tip_network,
            downloads: Box::pin(Downloads::new(block_network, verifier)),
            state,
            prospective_tips: HashSet::new(),
            recent_syncs,
        };

        (new_syncer, sync_length_receiver)
    }

    #[instrument(skip(self))]
    pub async fn sync(mut self) -> Result<(), Report> {
        // We can't download the genesis block using our normal algorithm,
        // due to protocol limitations
        self.request_genesis().await?;

        // Distinguishes a restart from a start, so we don't sleep when starting
        // the sync process, but we can keep restart logic in one place.
        let mut started_once = false;

        'sync: loop {
            if started_once {
                tracing::info!(timeout = ?SYNC_RESTART_DELAY, "waiting to restart sync");
                self.prospective_tips = HashSet::new();
                self.downloads.cancel_all();
                self.update_metrics();
                sleep(SYNC_RESTART_DELAY).await;
            } else {
                started_once = true;
            }

            tracing::info!("starting sync, obtaining new tips");
            if let Err(e) = self.obtain_tips().await {
                tracing::warn!(?e, "error obtaining tips");
                continue 'sync;
            }
            self.update_metrics();

            while !self.prospective_tips.is_empty() {
                // Check whether any block tasks are currently ready:
                while let Some(Some(rsp)) = self.downloads.next().now_or_never() {
                    match rsp {
                        Ok(hash) => {
                            tracing::trace!(?hash, "verified and committed block to state");
                        }
                        Err(e) => {
                            if format!("{:?}", e).contains("AlreadyVerified {") {
                                tracing::debug!(error = ?e, "block was already verified, possibly from a previous sync run, continuing");
                            } else {
                                tracing::warn!(?e, "error downloading and verifying block");
                                continue 'sync;
                            }
                        }
                    }
                }
                self.update_metrics();

                // If we have too many pending tasks, wait for some to finish.
                //
                // Starting to wait is interesting, but logging each wait can be
                // very verbose.
                if self.downloads.in_flight() > self.lookahead_limit {
                    tracing::info!(
                        tips.len = self.prospective_tips.len(),
                        in_flight = self.downloads.in_flight(),
                        lookahead_limit = self.lookahead_limit,
                        "waiting for pending blocks",
                    );
                }
                while self.downloads.in_flight() > self.lookahead_limit {
                    tracing::trace!(
                        tips.len = self.prospective_tips.len(),
                        in_flight = self.downloads.in_flight(),
                        lookahead_limit = self.lookahead_limit,
                        "waiting for pending blocks",
                    );

                    match self.downloads.next().await.expect("downloads is nonempty") {
                        Ok(hash) => {
                            tracing::trace!(?hash, "verified and committed block to state");
                        }

                        Err(e) => {
                            if format!("{:?}", e).contains("AlreadyVerified {") {
                                tracing::debug!(error = ?e, "block was already verified, possibly from a previous sync run, continuing");
                            } else {
                                tracing::warn!(?e, "error downloading and verifying block");
                                continue 'sync;
                            }
                        }
                    }
                    self.update_metrics();
                }

                // Once we're below the lookahead limit, we can keep extending the tips.
                tracing::info!(
                    tips.len = self.prospective_tips.len(),
                    in_flight = self.downloads.in_flight(),
                    lookahead_limit = self.lookahead_limit,
                    "extending tips",
                );

                if let Err(e) = self.extend_tips().await {
                    tracing::warn!(?e, "error extending tips");
                    continue 'sync;
                }
                self.update_metrics();
            }

            tracing::info!("exhausted prospective tip set");
        }
    }

    /// Given a block_locator list fan out request for subsequent hashes to
    /// multiple peers
    #[instrument(skip(self))]
    async fn obtain_tips(&mut self) -> Result<(), Report> {
        let block_locator = self
            .state
            .ready_and()
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

        tracing::info!(tip = ?block_locator.first().unwrap(), "trying to obtain new chain tips");
        tracing::debug!(?block_locator, "got block locator");

        let mut requests = FuturesUnordered::new();
        for _ in 0..FANOUT {
            requests.push(
                self.tip_network
                    .ready_and()
                    .await
                    .map_err(|e| eyre!(e))?
                    .call(zn::Request::FindBlocks {
                        known_blocks: block_locator.clone(),
                        stop: None,
                    }),
            );
        }

        let mut download_set = HashSet::new();
        while let Some(res) = requests.next().await {
            match res.map_err::<Report, _>(|e| eyre!(e)) {
                Ok(zn::Response::BlockHashes(hashes)) => {
                    tracing::trace!(?hashes);

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

                    tracing::debug!(hashes.len = ?hashes.len(), ?first_unknown);

                    let unknown_hashes = if let Some(index) = first_unknown {
                        &hashes[index..]
                    } else {
                        continue;
                    };

                    tracing::trace!(?unknown_hashes);

                    let new_tip = if let Some(end) = unknown_hashes.rchunks_exact(2).next() {
                        CheckedTip {
                            tip: end[0],
                            expected_next: end[1],
                        }
                    } else {
                        tracing::debug!("discarding response that extends only one block");
                        continue;
                    };

                    // Make sure we get the same tips, regardless of the
                    // order of peer responses
                    if !download_set.contains(&new_tip.expected_next) {
                        tracing::debug!(?new_tip,
                                        "adding new prospective tip, and removing existing tips in the new block hash list");
                        self.prospective_tips
                            .retain(|t| !unknown_hashes.contains(&t.expected_next));
                        self.prospective_tips.insert(new_tip);
                    } else {
                        tracing::debug!(
                            ?new_tip,
                            "discarding prospective tip: already in download set"
                        );
                    }

                    let prev_download_len = download_set.len();
                    download_set.extend(unknown_hashes);
                    let new_download_len = download_set.len();
                    let new_hashes = new_download_len - prev_download_len;
                    tracing::debug!(new_hashes, "added hashes to download set");
                    metrics::histogram!("sync.obtain.response.hash.count", new_hashes as u64);
                }
                Ok(_) => unreachable!("network returned wrong response"),
                // We ignore this error because we made multiple fanout requests.
                Err(e) => tracing::debug!(?e),
            }
        }

        tracing::debug!(?self.prospective_tips);

        // Check that the new tips we got are actually unknown.
        for hash in &download_set {
            tracing::debug!(?hash, "checking if state contains hash");
            if self.state_contains(*hash).await? {
                return Err(eyre!("queued download of hash behind our chain tip"));
            }
        }

        let new_downloads = download_set.len();
        tracing::debug!(new_downloads, "queueing new downloads");
        metrics::gauge!("sync.obtain.queued.hash.count", new_downloads as f64);

        // security: use the actual number of new downloads from all peers,
        // so a single trailing peer can't toggle our mempool
        self.recent_syncs.push_obtain_tips_length(new_downloads);

        self.request_blocks(download_set).await?;

        Ok(())
    }

    #[instrument(skip(self))]
    async fn extend_tips(&mut self) -> Result<(), Report> {
        let tips = std::mem::take(&mut self.prospective_tips);

        let mut download_set = HashSet::new();
        tracing::info!(tips = ?tips.len(), "trying to extend chain tips");
        for tip in tips {
            tracing::debug!(?tip, "asking peers to extend chain tip");
            let mut responses = FuturesUnordered::new();
            for _ in 0..FANOUT {
                responses.push(
                    self.tip_network
                        .ready_and()
                        .await
                        .map_err(|e| eyre!(e))?
                        .call(zn::Request::FindBlocks {
                            known_blocks: vec![tip.tip],
                            stop: None,
                        }),
                );
            }
            while let Some(res) = responses.next().await {
                match res.map_err::<Report, _>(|e| eyre!(e)) {
                    Ok(zn::Response::BlockHashes(hashes)) => {
                        tracing::debug!(first = ?hashes.first(), len = ?hashes.len());
                        tracing::trace!(?hashes);

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
                                tracing::debug!(?first_hash,
                                                ?tip.expected_next,
                                                ?tip.tip,
                                                "unexpected first hash, but the second matches: using the hashes after the match");
                                rest
                            }
                            // We ignore these responses
                            [] => continue,
                            [single_hash] => {
                                tracing::debug!(?single_hash,
                                                ?tip.expected_next,
                                                ?tip.tip,
                                                "discarding response containing a single unexpected hash");
                                continue;
                            }
                            [first_hash, second_hash, rest @ ..] => {
                                tracing::debug!(?first_hash,
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
                            tracing::debug!("discarding response that extends only one block");
                            continue;
                        };

                        tracing::trace!(?unknown_hashes);

                        // Make sure we get the same tips, regardless of the
                        // order of peer responses
                        if !download_set.contains(&new_tip.expected_next) {
                            tracing::debug!(?new_tip,
                                            "adding new prospective tip, and removing any existing tips in the new block hash list");
                            self.prospective_tips
                                .retain(|t| !unknown_hashes.contains(&t.expected_next));
                            self.prospective_tips.insert(new_tip);
                        } else {
                            tracing::debug!(
                                ?new_tip,
                                "discarding prospective tip: already in download set"
                            );
                        }

                        let prev_download_len = download_set.len();
                        download_set.extend(unknown_hashes);
                        let new_download_len = download_set.len();
                        let new_hashes = new_download_len - prev_download_len;
                        tracing::debug!(new_hashes, "added hashes to download set");
                        metrics::histogram!("sync.extend.response.hash.count", new_hashes as u64);
                    }
                    Ok(_) => unreachable!("network returned wrong response"),
                    // We ignore this error because we made multiple fanout requests.
                    Err(e) => tracing::debug!(?e),
                }
            }
        }

        let new_downloads = download_set.len();
        tracing::debug!(new_downloads, "queueing new downloads");
        metrics::gauge!("sync.extend.queued.hash.count", new_downloads as f64);

        // security: use the actual number of new downloads from all peers,
        // so a single trailing peer can't toggle our mempool
        self.recent_syncs.push_extend_tips_length(new_downloads);

        self.request_blocks(download_set).await?;

        Ok(())
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
            tracing::info!("starting genesis block download and verify");
            self.downloads
                .download_and_verify(self.genesis_hash)
                .await
                .map_err(|e| eyre!(e))?;
            match self.downloads.next().await.expect("downloads is nonempty") {
                Ok(hash) => tracing::trace!(?hash, "verified and committed block to state"),
                Err(e) => {
                    tracing::warn!(?e, "could not download or verify genesis block, retrying");
                    tokio::time::sleep(GENESIS_TIMEOUT_RETRY).await;
                }
            }
        }

        Ok(())
    }

    /// Queue download and verify tasks for each block that isn't currently known to our node
    async fn request_blocks(&mut self, hashes: HashSet<block::Hash>) -> Result<(), Report> {
        tracing::debug!(hashes.len = hashes.len(), "requesting blocks");
        for hash in hashes.into_iter() {
            self.downloads.download_and_verify(hash).await?;
        }

        Ok(())
    }

    /// Returns `true` if the hash is present in the state, and `false`
    /// if the hash is not present in the state.
    ///
    /// BUG: check if the hash is in any chain (#862)
    /// Depth only checks the main chain.
    async fn state_contains(&mut self, hash: block::Hash) -> Result<bool, Report> {
        match self
            .state
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::Depth(hash))
            .await
            .map_err(|e| eyre!(e))?
        {
            zs::Response::Depth(Some(_)) => Ok(true),
            zs::Response::Depth(None) => Ok(false),
            _ => unreachable!("wrong response to depth request"),
        }
    }

    fn update_metrics(&self) {
        metrics::gauge!(
            "sync.prospective_tips.len",
            self.prospective_tips.len() as f64
        );
        metrics::gauge!(
            "sync.downloads.in_flight",
            self.downloads.in_flight() as f64
        );
    }
}
