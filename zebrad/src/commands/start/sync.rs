use std::{collections::HashSet, iter, pin::Pin, sync::Arc, time::Duration};

use color_eyre::eyre::{eyre, Report};
use futures::stream::{FuturesUnordered, StreamExt};
use tokio::{task::JoinHandle, time::delay_for};
use tower::{retry::Retry, Service, ServiceExt};
use tracing_futures::{Instrument, Instrumented};

use zebra_chain::{
    block::{Block, BlockHeaderHash},
    Network,
};
use zebra_consensus::checkpoint;
use zebra_consensus::parameters;
use zebra_network::{self as zn, RetryLimit};
use zebra_state as zs;

/// The number of different peers we use to obtain and extend tips.
const FANOUT: usize = checkpoint::MAX_QUEUED_BLOCKS_PER_HEIGHT;
/// The maximum number of times we will retry a block download.
// TODO: Resolve the backpressure/checkpoint deadlock, and reduce the number of
// retries
const RETRY_LIMIT: usize = 10;

/// Controls how far ahead of the chain tip the syncer tries to download before
/// waiting for queued verifications to complete. Set to twice the maximum
/// checkpoint distance.
pub const LOOKAHEAD_LIMIT: usize = checkpoint::MAX_CHECKPOINT_HEIGHT_GAP * 2;

#[derive(Debug)]
pub struct Syncer<ZN, ZS, ZV>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = Error> + Send + Clone + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = Error> + Send + Clone + 'static,
    ZS::Future: Send,
    ZV: Service<Arc<Block>, Response = BlockHeaderHash, Error = Error> + Send + Clone + 'static,
    ZV::Future: Send,
{
    /// Used to perform ObtainTips and ExtendTips requests, with no retry logic.
    ///
    /// Failover is handled using fanout.
    tip_network: ZN,
    /// Used to download blocks, with retry logic.
    block_network: Retry<RetryLimit, ZN>,

    /// Used to check the currently verified blocks.
    state: ZS,
    /// Used to verify blocks.
    verifier: ZV,
    /// The hash of the genesis block for the configured `Network`.
    genesis_hash: BlockHeaderHash,

    /// The current set of tips that we're chasing.
    prospective_tips: HashSet<BlockHeaderHash>,

    /// A future for each block that is awaiting download and verification.
    pending_blocks:
        Pin<Box<FuturesUnordered<Instrumented<JoinHandle<Result<BlockHeaderHash, SyncError>>>>>>,
    /// The hashes for each future in pending_blocks.
    pending_hashes: HashSet<BlockHeaderHash>,
}

impl<ZN, ZS, ZV> Syncer<ZN, ZS, ZV>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = Error> + Send + Clone + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = Error> + Send + Clone + 'static,
    ZS::Future: Send,
    ZV: Service<Arc<Block>, Response = BlockHeaderHash, Error = Error> + Send + Clone + 'static,
    ZV::Future: Send,
{
    /// Returns a new syncer instance, using:
    ///  - chain: the zebra-chain `Network` to download (Mainnet or Testnet)
    ///  - peers: the zebra-network peers to contact for downloads
    ///  - state: the zebra-state that stores the chain
    ///  - verifier: the zebra-consensus verifier that checks the chain
    pub fn new(chain: Network, peers: ZN, state: ZS, verifier: ZV) -> Self {
        let retry_peers = Retry::new(RetryLimit::new(RETRY_LIMIT), peers.clone());
        Self {
            tip_network: peers,
            block_network: retry_peers,
            state,
            verifier,
            genesis_hash: parameters::genesis_hash(chain),
            prospective_tips: HashSet::new(),
            pending_blocks: Box::pin(FuturesUnordered::new()),
            pending_hashes: HashSet::new(),
        }
    }

    #[instrument(skip(self))]
    pub async fn sync(&mut self) -> Result<(), Report> {
        // We can't download the genesis block using our normal algorithm,
        // due to protocol limitations
        self.request_genesis().await?;

        loop {
            self.obtain_tips().await?;
            self.update_metrics();

            // ObtainTips Step 6
            //
            // If there are any prospective tips, call ExtendTips. Continue this step until there are no more prospective tips.
            while !self.prospective_tips.is_empty() {
                tracing::debug!("extending prospective tips");
                self.extend_tips().await?;
                self.update_metrics();

                // Check whether we need to wait for existing block download tasks to finish
                while self.pending_blocks.len() > LOOKAHEAD_LIMIT {
                    // TODO: handle backpressure/checkpoint deadlocks here
                    let _ = self.process_next_block().await;
                }

                // We just added a bunch of failures, update the metrics now,
                // because we might be about to reset or delay.
                self.update_metrics();
            }

            delay_for(Duration::from_secs(15)).await;
        }
    }

    /// Process the next block result.
    ///
    /// Handles download and verify failures by adding the block hash to the
    /// relevant list.
    #[instrument(skip(self))]
    async fn process_next_block(&mut self) -> Result<BlockHeaderHash, SyncError> {
        let result = self
            .pending_blocks
            .next()
            .await
            .expect("already checked there's at least one pending block task")
            .expect("block download and verify tasks should not panic");
        let hash = match result {
            Ok(hash) => {
                tracing::debug!(?hash, "verified and committed block to state");
                hash
            }
            // This is a non-transient error indicating either that
            // we've repeatedly missed a block we need or that we've
            // repeatedly missed a bad block suggested by a peer
            // feeding us bad hashes.
            //
            // TODO(hdevalence): handle interruptions in the chain
            // sync process. this should detect when we've stopped
            // making progress (probably using a timeout), then
            // continue the loop with a new invocation of
            // obtain_tips(), which will restart block downloads.
            Err((hash, ref e)) => {
                // Errors happen frequently on mainnet, due to bad peers.
                tracing::debug!(?e, "potentially transient error");
                hash
            }
        };
        if !self.pending_hashes.remove(&hash) {
            tracing::error!("Response for hash that wasn't pending: {:?}", hash);
        };

        result
    }

    /// Given a block_locator list fan out request for subsequent hashes to
    /// multiple peers
    #[instrument(skip(self))]
    async fn obtain_tips(&mut self) -> Result<(), Report> {
        // ObtainTips Step 1
        //
        // Query the current state to construct the sequence of hashes: handled by
        // the caller
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

        // ObtainTips Step 2
        //
        // Make a FindBlocksByHash request to the network F times, where F is a
        // fanout parameter, to get resp1, ..., respF
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
                Ok(zn::Response::BlockHeaderHashes(mut hashes)) => {
                    if hashes.is_empty() {
                        tracing::debug!("skipping empty response");
                        continue;
                    } else {
                        tracing::debug!(hashes.len = hashes.len(), "processing response");
                    }

                    // ObtainTips Step 3
                    //
                    // For each response, starting from the beginning of the
                    // list, prune any block hashes already included in the
                    // state, stopping at the first unknown hash to get resp1',
                    // ..., respF'. (These lists may be empty).

                    // Also prune any hashes that we're currently downloading,
                    // any that are already in our list for the next download,
                    // and any that have failed recently.
                    self.remove_duplicates(&mut hashes, &download_set);

                    let mut first_unknown = None;
                    for (i, &hash) in hashes.iter().enumerate() {
                        if self.get_depth(hash).await?.is_none() {
                            first_unknown = Some(i);
                            break;
                        }
                    }

                    // Hashes will be empty if peers give identical responses,
                    // or we know about all the blocks in their response.
                    if first_unknown.is_none() {
                        tracing::debug!("ObtainTips: all hashes are known");
                        continue;
                    }
                    let first_unknown = first_unknown.expect("already checked for None");
                    tracing::debug!(
                        first_unknown,
                        "found index of first unknown hash in response"
                    );

                    let unknown_hashes = &hashes[first_unknown..];
                    let new_tip = *unknown_hashes
                        .last()
                        .expect("enumerate returns a valid index");

                    // Check for tips we've already seen
                    // TODO: remove this check once the sync service is more reliable
                    let depth = self.get_depth(new_tip).await?;
                    if let Some(depth) = depth {
                        tracing::warn!(
                            ?depth,
                            ?new_tip,
                            "ObtainTips: Unexpected duplicate tip from peer: already in state"
                        );
                        continue;
                    }

                    // ObtainTips Step 4:
                    // Combine the last elements of each list into a set; this is the
                    // set of prospective tips.
                    tracing::debug!(hashes.len = ?hashes.len(), ?new_tip, "ObtainTips: adding new prospective tip");
                    self.prospective_tips.insert(new_tip);

                    // ObtainTips Step 5.1
                    //
                    // Combine all elements of each list into a set
                    let prev_download_len = download_set.len();
                    download_set.extend(unknown_hashes);
                    let new_download_len = download_set.len();
                    tracing::debug!(
                        prev_download_len,
                        new_download_len,
                        new_hashes = new_download_len - prev_download_len,
                        "ObtainTips: added hashes to download set"
                    );
                }
                Ok(_) => unreachable!("network returned wrong response"),
                // We ignore this error because we made multiple fanout requests.
                Err(e) => tracing::debug!(?e),
            }
        }

        tracing::debug!(?self.prospective_tips, "ObtainTips: downloading blocks for tips");

        // ObtainTips Step 5.2
        //
        // queue download and verification of those blocks.
        self.request_blocks(download_set.into_iter().collect())
            .await?;

        Ok(())
    }

    #[instrument(skip(self))]
    async fn extend_tips(&mut self) -> Result<(), Report> {
        // Extend Tips 1
        //
        // remove all prospective tips and iterate over them individually
        let tips = std::mem::take(&mut self.prospective_tips);
        tracing::debug!(?tips, "extending tip set");

        let mut download_set = HashSet::new();
        for tip in tips {
            // ExtendTips Step 2
            //
            // Create a FindBlocksByHash request consisting of just the
            // prospective tip. Send this request to the network F times
            let mut responses = FuturesUnordered::new();
            for _ in 0..FANOUT {
                responses.push(
                    self.tip_network
                        .ready_and()
                        .await
                        .map_err(|e| eyre!(e))?
                        .call(zn::Request::FindBlocks {
                            known_blocks: vec![tip],
                            stop: None,
                        }),
                );
            }
            while let Some(res) = responses.next().await {
                match res.map_err::<Report, _>(|e| eyre!(e)) {
                    Ok(zn::Response::BlockHeaderHashes(mut hashes)) => {
                        // ExtendTips Step 3
                        //
                        // For each response, check whether the first hash in the
                        // response is the genesis block; if so, discard the response.
                        // It indicates that the remote peer does not have any blocks
                        // following the prospective tip.
                        match (hashes.first(), hashes.len()) {
                            (None, _) => {
                                tracing::debug!("ExtendTips: skipping empty response");
                                continue;
                            }
                            (_, 1) => {
                                tracing::debug!("ExtendTips: skipping length-1 response, in case it's an unsolicited inv message");
                                continue;
                            }
                            (Some(hash), _) if (hash == &self.genesis_hash) => {
                                tracing::debug!(
                                    "ExtendTips: skipping response, peer could not extend the tip"
                                );
                                continue;
                            }
                            (Some(&hash), _) => {
                                // Check for hashes we've already seen.
                                // This happens a lot near the end of the chain.
                                let depth = self.get_depth(hash).await?;
                                if let Some(depth) = depth {
                                    tracing::debug!(
                                        ?depth,
                                        ?hash,
                                        "ExtendTips: skipping response, peer returned a duplicate hash: already in state"
                                    );
                                    continue;
                                }
                            }
                        }

                        self.remove_duplicates(&mut hashes, &download_set);

                        let new_tip = match hashes.pop() {
                            Some(tip) => tip,
                            None => continue,
                        };

                        // Check for tips we've already seen
                        // TODO: remove this check once the sync service is more reliable
                        let depth = self.get_depth(new_tip).await?;
                        if let Some(depth) = depth {
                            tracing::warn!(
                                ?depth,
                                ?new_tip,
                                "ExtendTips: Unexpected duplicate tip from peer: already in state"
                            );
                            continue;
                        }

                        // ExtendTips Step 4
                        //
                        // Combine the last elements of the remaining responses into
                        // a set, and add this set to the set of prospective tips.
                        tracing::debug!(hashes.len = ?hashes.len(), ?new_tip, "ExtendTips: extending to new tip");
                        let _ = self.prospective_tips.insert(new_tip);

                        let prev_download_len = download_set.len();
                        download_set.extend(hashes);
                        let new_download_len = download_set.len();
                        tracing::debug!(
                            prev_download_len,
                            new_download_len,
                            new_hashes = new_download_len - prev_download_len,
                            "ExtendTips: added hashes to download set"
                        );
                    }
                    Ok(_) => unreachable!("network returned wrong response"),
                    // We ignore this error because we made multiple fanout requests.
                    Err(e) => tracing::debug!("{:?}", e),
                }
            }
        }

        tracing::debug!(?self.prospective_tips, "ExtendTips: downloading blocks for tips");

        // ExtendTips Step 5
        //
        // Combine all elements of the remaining responses into a
        // set, and queue download and verification of those blocks
        self.request_blocks(
            download_set
                .into_iter()
                .chain(self.prospective_tips.iter().cloned())
                .collect(),
        )
        .await?;

        Ok(())
    }

    /// Queue a download for the genesis block, if it isn't currently known to
    /// our node.
    #[instrument(skip(self))]
    async fn request_genesis(&mut self) -> Result<(), Report> {
        // Due to Bitcoin protocol limitations, we can't request the genesis
        // block using our standard tip-following algorithm:
        //  - getblocks requires at least one hash
        //  - responses start with the block *after* the requested block, and
        //  - the genesis hash is used as a placeholder for "no matches".
        //
        // So we just queue the genesis block here.

        let state_has_genesis = self.get_depth(self.genesis_hash).await?.is_some();
        if !state_has_genesis {
            self.request_blocks(vec![self.genesis_hash]).await?;
        }

        Ok(())
    }

    /// Queue downloads for each block that isn't currently known to our node
    async fn request_blocks(&mut self, hashes: Vec<BlockHeaderHash>) -> Result<(), Report> {
        tracing::debug!(hashes.len = hashes.len(), "requesting blocks");
        for hash in hashes.into_iter() {
            // TODO: remove this check once the sync service is more reliable
            let depth = self.get_depth(hash).await?;
            if let Some(depth) = depth {
                tracing::warn!(
                    ?depth,
                    ?hash,
                    "request_blocks: Unexpected duplicate hash: already in state"
                );
                continue;
            }
            // We construct the block download requests sequentially, waiting
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
            let span = tracing::info_span!("block_fetch_verify", ?hash);
            let mut verifier = self.verifier.clone();
            let task = tokio::spawn(async move {
                let block = match block_req.await {
                    Ok(zn::Response::Blocks(blocks)) => blocks
                        .into_iter()
                        .next()
                        .expect("successful response has the block in it"),
                    Ok(_) => unreachable!("wrong response to block request"),
                    Err(e) => Err((hash, e))?,
                };
                metrics::counter!("sync.downloaded_blocks", 1);

                let response = verifier
                    .ready_and()
                    .await
                    .map_err(|e| (hash, e))?
                    .call(block)
                    .await
                    .map_err(|e| (hash, e));
                metrics::counter!("sync.verified_blocks", 1);
                response
            })
            .instrument(span);
            self.pending_blocks.push(task);
            if !self.pending_hashes.insert(hash) {
                unreachable!("Callers should filter out pending hashes before requesting blocks.");
            }
        }

        Ok(())
    }

    /// Get the depth of hash in the current chain.
    ///
    /// Returns None if the hash is not present in the chain.
    /// TODO: handle multiple tips in the state.
    #[instrument(skip(self))]
    async fn get_depth(&mut self, hash: BlockHeaderHash) -> Result<Option<u32>, Report> {
        match self
            .state
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::GetDepth { hash })
            .await
            .map_err(|e| eyre!(e))?
        {
            zs::Response::Depth(d) => Ok(d),
            _ => unreachable!("wrong response to depth request"),
        }
    }

    /// Filter duplicates out of `latest_hashes`, using the recently processed
    /// hashes `recent_hashes`, and the pending and failed hashes.
    fn remove_duplicates(
        &self,
        latest_hashes: &mut Vec<BlockHeaderHash>,
        recent_hashes: &HashSet<BlockHeaderHash>,
    ) {
        latest_hashes
            .retain(|hash| !recent_hashes.contains(hash) && !self.pending_hashes.contains(hash));
    }

    /// Update metrics gauges, and create a trace containing metrics.
    fn update_metrics(&self) {
        metrics::gauge!(
            "sync.prospective_tips.len",
            self.prospective_tips.len() as i64
        );
        metrics::gauge!("sync.pending_blocks.len", self.pending_blocks.len() as i64);
        tracing::info!(
            tips.len = self.prospective_tips.len(),
            pending.len = self.pending_blocks.len(),
            pending.limit = LOOKAHEAD_LIMIT,
        );
    }
}

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
type SyncError = (BlockHeaderHash, Error);
