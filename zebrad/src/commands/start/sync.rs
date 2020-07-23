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

// XXX in the future, we may not be able to access the checkpoint module.
const FANOUT: usize = checkpoint::MAX_QUEUED_BLOCKS_PER_HEIGHT;
/// Controls how far ahead of the chain tip the syncer tries to download before
/// waiting for queued verifications to complete. Set to twice the maximum
/// checkpoint distance.
const LOOKAHEAD_LIMIT: usize = 2 * 2_000;

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
    /// Used to perform extendtips requests, with no retry logic (failover is handled using fanout).
    tip_network: ZN,
    /// Used to download blocks, with retry logic.
    block_network: Retry<RetryLimit, ZN>,
    state: ZS,
    verifier: ZV,
    prospective_tips: HashSet<BlockHeaderHash>,
    pending_blocks:
        Pin<Box<FuturesUnordered<Instrumented<JoinHandle<Result<BlockHeaderHash, Error>>>>>>,
    genesis_hash: BlockHeaderHash,
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
        let retry_peers = Retry::new(RetryLimit::new(3), peers.clone());
        Self {
            tip_network: peers,
            block_network: retry_peers,
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

        loop {
            self.obtain_tips().await?;
            metrics::gauge!(
                "sync.prospective_tips.len",
                self.prospective_tips.len() as i64
            );
            metrics::gauge!("sync.pending_blocks.len", self.pending_blocks.len() as i64);

            // ObtainTips Step 6
            //
            // If there are any prospective tips, call ExtendTips. Continue this step until there are no more prospective tips.
            while !self.prospective_tips.is_empty() {
                tracing::debug!("extending prospective tips");

                self.extend_tips().await?;

                metrics::gauge!(
                    "sync.prospective_tips.len",
                    self.prospective_tips.len() as i64
                );
                metrics::gauge!("sync.pending_blocks.len", self.pending_blocks.len() as i64);
                tracing::debug!(
                    pending.len = self.pending_blocks.len(),
                    limit = LOOKAHEAD_LIMIT
                );

                // Check whether we need to wait for existing block download tasks to finish
                while self.pending_blocks.len() > LOOKAHEAD_LIMIT {
                    match self
                        .pending_blocks
                        .next()
                        .await
                        .expect("already checked there's at least one pending block task")
                        .expect("block download tasks should not panic")
                    {
                        Ok(hash) => tracing::debug!(?hash, "verified and committed block to state"),
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
                        // this requires correctly constructing a block locator
                        // (TODO below) and ensuring that the verifier handles
                        // multiple requests for verification of the same block
                        // hash by handling both requests or by discarding the
                        // earlier request in favor of the later one.
                        Err(e) => tracing::error!(?e, "potentially transient error"),
                    };
                }
            }

            delay_for(Duration::from_secs(15)).await;
        }
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

        tracing::info!(?block_locator, "trying to obtain new chain tips");

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
                Ok(zn::Response::BlockHeaderHashes(hashes)) => {
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
                    let mut first_unknown = 0;
                    for (i, &hash) in hashes.iter().enumerate() {
                        let depth = self
                            .state
                            .ready_and()
                            .await
                            .map_err(|e| eyre!(e))?
                            .call(zebra_state::Request::GetDepth { hash })
                            .await
                            .map_err(|e| eyre!(e))?;
                        if let zs::Response::Depth(None) = depth {
                            first_unknown = i;
                            break;
                        }
                    }
                    tracing::debug!(
                        first_unknown,
                        "found index of first unknown hash in response"
                    );
                    if first_unknown == hashes.len() {
                        // We should only stop getting hashes once we've finished the initial sync
                        tracing::debug!("no new hashes, even though we gave our tip?");
                        continue;
                    }

                    let unknown_hashes = &hashes[first_unknown..];
                    let new_tip = *unknown_hashes
                        .last()
                        .expect("already checked first_unknown < hashes.len()");

                    // ObtainTips Step 4:
                    // Combine the last elements of each list into a set; this is the
                    // set of prospective tips.
                    if !download_set.contains(&new_tip) {
                        tracing::debug!(?new_tip, "adding new prospective tip");
                        self.prospective_tips.insert(new_tip);
                    } else {
                        tracing::debug!(?new_tip, "discarding tip already queued for download");
                    }

                    let prev_download_len = download_set.len();
                    download_set.extend(unknown_hashes);
                    let new_download_len = download_set.len();
                    tracing::debug!(
                        prev_download_len,
                        new_download_len,
                        new_hashes = new_download_len - prev_download_len,
                        "added hashes to download set"
                    );
                }
                Ok(_) => unreachable!("network returned wrong response"),
                // We ignore this error because we made multiple fanout requests.
                Err(e) => tracing::debug!(?e),
            }
        }

        // ObtainTips Step 5
        //
        // Combine all elements of each list into a set, and queue
        // download and verification of those blocks.
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
                            (_, 0) => {
                                tracing::debug!("skipping empty response");
                                continue;
                            }
                            (_, 1) => {
                                tracing::debug!("skipping length-1 response, in case it's an unsolicited inv message");
                                continue;
                            }
                            (Some(hash), _) if (hash == &self.genesis_hash) => {
                                tracing::debug!("skipping response, peer could not extend the tip");
                                continue;
                            }
                            _ => {}
                        }

                        let new_tip = hashes.pop().expect("expected: hashes must have len > 0");

                        // ExtendTips Step 4
                        //
                        // Combine the last elements of the remaining responses into
                        // a set, and add this set to the set of prospective tips.
                        tracing::debug!(?new_tip, hashes.len = ?hashes.len());
                        let _ = self.prospective_tips.insert(new_tip);

                        download_set.extend(hashes);
                    }
                    Ok(_) => unreachable!("network returned wrong response"),
                    // We ignore this error because we made multiple fanout requests.
                    Err(e) => tracing::debug!("{:?}", e),
                }
            }
        }

        // ExtendTips Step ??
        //
        // Remove tips that are already included behind one of the other
        // returned tips
        self.prospective_tips
            .retain(|tip| !download_set.contains(tip));

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
    async fn request_genesis(&mut self) -> Result<(), Report> {
        // Due to Bitcoin protocol limitations, we can't request the genesis
        // block using our standard tip-following algorithm:
        //  - getblocks requires at least one hash
        //  - responses start with the block *after* the requested block, and
        //  - the genesis hash is used as a placeholder for "no matches".
        //
        // So we just queue the genesis block here.

        let state_has_genesis = self
            .state
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::GetBlock {
                hash: self.genesis_hash,
            })
            .await
            .is_ok();

        if !state_has_genesis {
            self.request_blocks(vec![self.genesis_hash]).await?;
        }

        Ok(())
    }

    /// Queue downloads for each block that isn't currently known to our node
    async fn request_blocks(&mut self, hashes: Vec<BlockHeaderHash>) -> Result<(), Report> {
        tracing::debug!(hashes.len = hashes.len(), "requesting blocks");
        for hash in hashes.into_iter() {
            let mut retry_peer_set = self.block_network.clone();
            let mut verifier = self.verifier.clone();
            let span = tracing::info_span!("block_fetch_verify", ?hash);
            let task = tokio::spawn(async move {
                let block = match retry_peer_set
                    .ready_and()
                    .await?
                    .call(zn::Request::BlocksByHash(iter::once(hash).collect()))
                    .await
                {
                    Ok(zn::Response::Blocks(blocks)) => blocks
                        .into_iter()
                        .next()
                        .expect("successful response has the block in it"),
                    Ok(_) => unreachable!("wrong response to block request"),
                    Err(e) => return Err(e),
                };
                metrics::counter!("sync.downloaded_blocks", 1);

                verifier.ready_and().await?.call(block).await
            })
            .instrument(span);
            self.pending_blocks.push(task);
        }

        Ok(())
    }
}

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
