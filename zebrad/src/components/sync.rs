use std::{collections::HashSet, iter, pin::Pin, sync::Arc, time::Duration};

use color_eyre::eyre::{eyre, Report, WrapErr};
use futures::{
    future::{FutureExt, TryFutureExt},
    stream::{FuturesUnordered, StreamExt},
};
use tokio::{task::JoinHandle, time::delay_for};
use tower::{builder::ServiceBuilder, retry::Retry, timeout::Timeout, Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::{
    block::{self, Block},
    parameters::Network,
};
use zebra_consensus::{checkpoint, parameters};
use zebra_network as zn;
use zebra_state as zs;

/// Controls the number of peers used for each ObtainTips and ExtendTips request.
// XXX in the future, we may not be able to access the checkpoint module.
const FANOUT: usize = checkpoint::MAX_QUEUED_BLOCKS_PER_HEIGHT;
/// Controls how many times we will retry each block download.
///
/// If all the retries fail, then the syncer will reset, and start downloading
/// blocks from the verified tip in the state, including blocks which previously
/// downloaded successfully.
///
/// But if a node is on a slow or unreliable network, sync restarts can result
/// in a flood of download requests, making future syncs more likely to fail.
/// So it's much faster to retry each block multiple times.
///
/// When we implement a peer reputation system, we can reduce the number of
/// retries, because we will be more likely to choose a good peer.
const BLOCK_DOWNLOAD_RETRY_LIMIT: usize = 5;

/// Controls how far ahead of the chain tip the syncer tries to download before
/// waiting for queued verifications to complete. Set to twice the maximum
/// checkpoint distance.
///
/// Some checkpoints contain larger blocks, so the maximum checkpoint gap can
/// represent multiple gigabytes of data.
const LOOKAHEAD_LIMIT: usize = checkpoint::MAX_CHECKPOINT_HEIGHT_GAP * 2;

/// Controls how long we wait for a tips response to return.
///
/// The network layer also imposes a timeout on requests.
const TIPS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(6);
/// Controls how long we wait for a block download request to complete.
///
/// The network layer also imposes a timeout on requests.
const BLOCK_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(20);

/// The maximum amount of time that Zebra should take to download a checkpoint
/// full of blocks. Based on the current `MAX_CHECKPOINT_BYTE_SIZE`.
///
/// We assume that Zebra nodes have at least 10 Mbps bandwidth, and allow some
/// extra time for request latency.
const MAX_CHECKPOINT_DOWNLOAD_SECONDS: u64 = 300;

/// Controls how long we wait for a block verify task to complete.
///
/// This timeout makes sure that the syncer and verifiers do not deadlock.
/// When the `LOOKAHEAD_LIMIT` is reached, the syncer waits for blocks to verify
/// (or fail). If the verifiers are also waiting for more blocks from the syncer,
/// then without a timeout, Zebra would deadlock.
const BLOCK_VERIFY_TIMEOUT: Duration = Duration::from_secs(MAX_CHECKPOINT_DOWNLOAD_SECONDS);

/// Controls how long we wait to retry ExtendTips after it fails.
///
/// This timeout should be long enough to allow some of our peers to clear
/// their connection state.
///
/// (ObtainTips failures use the sync restart timeout.)
const TIPS_RETRY_TIMEOUT: Duration = Duration::from_secs(60);
/// Controls how long we wait to restart syncing after finishing a sync run.
///
/// This timeout should be long enough to:
///   - allow pending downloads and verifies to complete or time out.
///     Sync restarts don't cancel downloads, so quick restarts can overload
///     network-bound nodes with lots of peers, leading to further failures.
///     (The total number of requests being processed by peers is the sum of
///     the number of peers, and the peer request buffer size.)
///
///     We assume that Zebra nodes have at least 10 Mbps bandwidth. So a
///     maximum-sized block can take up to 2 seconds to download. Therefore, we
///     set this timeout to twice the default number of peers. (The peer request
///     buffer size is small enough that any buffered requests will overlap with
///     the post-restart ObtainTips.)
///
///   - allow zcashd peers to process pending requests. If the node only has a
///     few peers, we want to clear as much peer state as possible. In
///     particular, zcashd sends "next block range" hints, based on zcashd's
///     internal model of our sync progress. But we want to discard these hints,
///     so they don't get confused with ObtainTips and ExtendTips responses.
///
/// This timeout is particularly important on instances with slow or unreliable
/// networks, and on testnet, which has a small number of slow peers.
const SYNC_RESTART_TIMEOUT: Duration = Duration::from_secs(100);

/// Helps work around defects in the bitcoin protocol by checking whether
/// the returned hashes actually extend a chain tip.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
struct CheckedTip {
    tip: block::Hash,
    expected_next: block::Hash,
}

#[derive(Debug)]
pub struct ChainSync<ZN, ZS, ZV>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = Error> + Send + Clone + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = Error> + Send + Clone + 'static,
    ZS::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = Error> + Send + Clone + 'static,
    ZV::Future: Send,
{
    /// Used to perform ObtainTips and ExtendTips requests, with no retry logic
    /// (failover is handled using fanout).
    tip_network: Timeout<ZN>,
    /// Used to download blocks, with retry logic.
    block_network: Retry<zn::RetryLimit, Timeout<ZN>>,
    state: ZS,
    verifier: Timeout<ZV>,
    prospective_tips: HashSet<CheckedTip>,
    pending_blocks: Pin<Box<FuturesUnordered<JoinHandle<Result<block::Hash, ReportAndHash>>>>>,
    genesis_hash: block::Hash,
}

impl<ZN, ZS, ZV> ChainSync<ZN, ZS, ZV>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = Error> + Send + Clone + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = Error> + Send + Clone + 'static,
    ZS::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = Error> + Send + Clone + 'static,
    ZV::Future: Send,
{
    /// Returns a new syncer instance, using:
    ///  - chain: the zebra-chain `Network` to download (Mainnet or Testnet)
    ///  - peers: the zebra-network peers to contact for downloads
    ///  - state: the zebra-state that stores the chain
    ///  - verifier: the zebra-consensus verifier that checks the chain
    pub fn new(chain: Network, peers: ZN, state: ZS, verifier: ZV) -> Self {
        let tip_network = Timeout::new(peers.clone(), TIPS_RESPONSE_TIMEOUT);
        let block_network = ServiceBuilder::new()
            .retry(zn::RetryLimit::new(BLOCK_DOWNLOAD_RETRY_LIMIT))
            .timeout(BLOCK_DOWNLOAD_TIMEOUT)
            .service(peers);
        let verifier = Timeout::new(verifier, BLOCK_VERIFY_TIMEOUT);
        Self {
            tip_network,
            block_network,
            state,
            verifier,
            prospective_tips: HashSet::new(),
            pending_blocks: Box::pin(FuturesUnordered::new()),
            genesis_hash: parameters::genesis_hash(chain),
        }
    }

    #[instrument(skip(self))]
    pub async fn sync(&mut self) -> Result<(), Report> {
        // We can't download the genesis block using our normal algorithm,
        // due to protocol limitations
        self.request_genesis().await?;

        'sync: loop {
            // Update metrics for any ready tasks, before wiping state
            while let Some(Some(rsp)) = self.pending_blocks.next().now_or_never() {
                match rsp.expect("block download and verify tasks should not panic") {
                    Ok(hash) => tracing::trace!(?hash, "verified and committed block to state"),
                    Err((e, _)) => {
                        tracing::trace!(?e, "sync error before restarting sync, ignoring")
                    }
                }
            }
            self.update_metrics();

            // Wipe state from prevous iterations.
            self.prospective_tips = HashSet::new();
            self.pending_blocks = Box::pin(FuturesUnordered::new());
            self.update_metrics();

            tracing::info!("starting sync, obtaining new tips");
            if self.obtain_tips().await.is_err() || self.prospective_tips.is_empty() {
                self.update_metrics();
                tracing::warn!("failed to obtain tips, waiting to restart sync");
                delay_for(SYNC_RESTART_TIMEOUT).await;
                continue 'sync;
            };

            while !self.prospective_tips.is_empty() {
                // Check whether any block tasks are currently ready:
                while let Some(Some(rsp)) = self.pending_blocks.next().now_or_never() {
                    match rsp.expect("block download and verify tasks should not panic") {
                        Ok(hash) => {
                            tracing::trace!(?hash, "verified and committed block to state");
                        }
                        Err((e, hash)) => {
                            // We must restart the sync on every error, unless
                            // this block has already been verified.
                            //
                            // If we ignore other errors, the syncer can:
                            //   - get a long way ahead of the state, and queue
                            //     up a lot of unverified blocks in memory, or
                            //   - get into an endless error cycle.
                            //
                            // In particular, we must restart if the checkpoint
                            // verifier has verified a block at this height, but
                            // the hash is different. In that case, we want to
                            // stop following an ancient side-chain.
                            if self.state_contains(hash).await? {
                                tracing::debug!(?e,
                                                "sync error in ready task, but block is already verified, ignoring");
                            } else {
                                tracing::warn!(
                                    ?e,
                                    "sync error in ready task, waiting to restart sync"
                                );
                                delay_for(SYNC_RESTART_TIMEOUT).await;
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
                if self.pending_blocks.len() > LOOKAHEAD_LIMIT {
                    tracing::info!(
                        tips.len = self.prospective_tips.len(),
                        pending.len = self.pending_blocks.len(),
                        pending.limit = LOOKAHEAD_LIMIT,
                        "waiting for pending blocks",
                    );
                }
                while self.pending_blocks.len() > LOOKAHEAD_LIMIT {
                    tracing::trace!(
                        tips.len = self.prospective_tips.len(),
                        pending.len = self.pending_blocks.len(),
                        pending.limit = LOOKAHEAD_LIMIT,
                        "continuing to wait for pending blocks",
                    );
                    match self
                        .pending_blocks
                        .next()
                        .await
                        .expect("pending_blocks is nonempty")
                        .expect("block download and verify tasks should not panic")
                    {
                        Ok(hash) => {
                            tracing::trace!(?hash, "verified and committed block to state");
                        }
                        Err((e, hash)) => {
                            // We must restart the sync on every error, unless
                            // this block has already been verified.
                            // See the comment above for details.
                            if self.state_contains(hash).await? {
                                tracing::debug!(?e,
                                                "sync error with pending above lookahead limit, but block is already verified, ignoring");
                            } else {
                                tracing::warn!(?e,
                                               "sync error with pending above lookahead limit, waiting to restart sync");
                                delay_for(SYNC_RESTART_TIMEOUT).await;
                                continue 'sync;
                            }
                        }
                    }
                    self.update_metrics();
                }

                // Once we're below the lookahead limit, we can keep extending the tips.
                tracing::info!(
                    tips.len = self.prospective_tips.len(),
                    pending.len = self.pending_blocks.len(),
                    pending.limit = LOOKAHEAD_LIMIT,
                    "extending tips",
                );
                let old_tips = self.prospective_tips.clone();
                let _ = self.extend_tips().await;

                // If ExtendTips fails, wait, then give it another shot.
                //
                // If we don't have many peers, waiting and retrying helps us
                // ignore unsolicited BlockHashes from peers.
                if self.prospective_tips.is_empty() {
                    self.update_metrics();
                    tracing::info!("no new tips, waiting to retry extend tips");
                    delay_for(TIPS_RETRY_TIMEOUT).await;
                    self.prospective_tips = old_tips;
                    let _ = self.extend_tips().await;
                }
                self.update_metrics();
            }

            tracing::info!("exhausted tips, waiting to restart sync");
            delay_for(SYNC_RESTART_TIMEOUT).await;
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
            .call(zebra_state::Request::GetBlockLocator {
                genesis: self.genesis_hash,
            })
            .await
            .map(|response| match response {
                zebra_state::Response::BlockLocator { block_locator } => block_locator,
                _ => unreachable!(
                    "GetBlockLocator request can only result in Response::BlockLocator"
                ),
            })
            .map_err(|e| eyre!(e))?;

        tracing::debug!(?block_locator, "trying to obtain new chain tips");

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
                    tracing::debug!(
                        new_hashes = new_download_len - prev_download_len,
                        "added hashes to download set"
                    );
                }
                Ok(_) => unreachable!("network returned wrong response"),
                // We ignore this error because we made multiple fanout requests.
                Err(e) => tracing::debug!(?e),
            }
        }

        tracing::debug!(?self.prospective_tips);

        self.request_blocks(download_set.into_iter().collect())
            .await?;

        Ok(())
    }

    #[instrument(skip(self))]
    async fn extend_tips(&mut self) -> Result<(), Report> {
        let tips = std::mem::take(&mut self.prospective_tips);

        let mut download_set = HashSet::new();
        for tip in tips {
            tracing::debug!(?tip, "extending tip");
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
                        tracing::debug!(
                            new_hashes = new_download_len - prev_download_len,
                            "added hashes to download set"
                        );
                    }
                    Ok(_) => unreachable!("network returned wrong response"),
                    // We ignore this error because we made multiple fanout requests.
                    Err(e) => tracing::debug!("{:?}", e),
                }
            }
        }

        self.request_blocks(download_set.into_iter().collect())
            .await?;

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
            self.request_blocks(vec![self.genesis_hash]).await?;
            match self
                .pending_blocks
                .next()
                .await
                .expect("inserted a download and verify request")
                .expect("block download and verify tasks should not panic")
            {
                Ok(hash) => tracing::trace!(?hash, "verified and committed block to state"),
                Err((e, _)) => {
                    tracing::warn!(?e, "could not download or verify genesis block, retrying")
                }
            }
        }

        Ok(())
    }

    /// Queue download and verify tasks for each block that isn't currently known to our node
    async fn request_blocks(&mut self, hashes: Vec<block::Hash>) -> Result<(), Report> {
        tracing::debug!(hashes.len = hashes.len(), "requesting blocks");
        for hash in hashes.into_iter() {
            // Avoid re-downloading blocks that have already been verified.
            // This is particularly important for nodes on slow or unreliable
            // networks.
            if self.state_contains(hash).await? {
                tracing::debug!(
                    ?hash,
                    "request_blocks: Unexpected duplicate hash: already in state"
                );
                continue;
            }
            // We construct the block requests sequentially, waiting
            // for the peer set to be ready to process each request. This
            // ensures that we start block downloads in the order we want them
            // (though they may resolve out of order), and it means that we
            // respect backpressure. Otherwise, if we waited for readiness and
            // did the service call in the spawned tasks, all of the spawned
            // tasks would race each other waiting for the network to become
            // ready.
            let block_req = self
                .block_network
                .ready_and()
                .await
                .map_err(|e| eyre!(e))?
                .call(zn::Request::BlocksByHash(iter::once(hash).collect()));

            tracing::trace!(?hash, "requested block");

            // This span is used to help diagnose sync warnings
            let span = tracing::warn_span!("block_fetch_verify", ?hash);
            let mut verifier = self.verifier.clone();
            let task = tokio::spawn(
                async move {
                    let block = match block_req.await {
                        Ok(zn::Response::Blocks(blocks)) => blocks
                            .into_iter()
                            .next()
                            .expect("successful response has the block in it"),
                        Ok(_) => unreachable!("wrong response to block request"),
                        // Make sure we can distinguish download and verify timeouts
                        Err(e) => Err(eyre!(e)).wrap_err("failed to download block")?,
                    };
                    metrics::counter!("sync.downloaded.block.count", 1);

                    let result = verifier
                        .ready_and()
                        .await
                        .map_err(|e| eyre!(e))
                        .wrap_err("verifier service failed to be ready")?
                        .call(block)
                        .await
                        .map_err(|e| eyre!(e))
                        .wrap_err("failed to verify block")?;
                    metrics::counter!("sync.verified.block.count", 1);
                    Result::<block::Hash, Report>::Ok(result)
                }
                .instrument(span)
                .map_err(move |e| (e, hash)),
            );
            self.pending_blocks.push(task);
        }

        Ok(())
    }

    /// Returns `true` if the hash is present in the state, and `false`
    /// if the hash is not present in the state.
    ///
    /// TODO: handle multiple tips in the state.
    async fn state_contains(&mut self, hash: block::Hash) -> Result<bool, Report> {
        match self
            .state
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::GetDepth { hash })
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
            self.prospective_tips.len() as i64
        );
        metrics::gauge!("sync.pending_blocks.len", self.pending_blocks.len() as i64);
    }
}

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
type ReportAndHash = (Report, block::Hash);

#[cfg(test)]
mod test {
    use super::*;

    /// Make sure the timeout values are consistent with each other.
    #[test]
    fn ensure_timeouts_consistent() {
        let max_download_retry_time =
            BLOCK_DOWNLOAD_TIMEOUT.as_secs() * (BLOCK_DOWNLOAD_RETRY_LIMIT as u64);
        assert!(
            max_download_retry_time < BLOCK_VERIFY_TIMEOUT.as_secs(),
            "Verify timeout should allow for previous block download retries"
        );
        assert!(
            BLOCK_DOWNLOAD_TIMEOUT.as_secs() * 2 < SYNC_RESTART_TIMEOUT.as_secs(),
            "Sync restart should allow for pending and buffered requests to complete"
        );

        assert!(
            TIPS_RETRY_TIMEOUT < BLOCK_VERIFY_TIMEOUT,
            "Verify timeout should allow for retrying tips"
        );
        assert!(
            SYNC_RESTART_TIMEOUT < BLOCK_VERIFY_TIMEOUT,
            "Verify timeout should allow for a sync restart"
        );
    }
}
