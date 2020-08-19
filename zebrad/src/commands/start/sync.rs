use std::{collections::HashSet, iter, pin::Pin, sync::Arc, time::Duration};

use color_eyre::eyre::{eyre, Report};
use futures::future::FutureExt;
use futures::stream::{FuturesUnordered, StreamExt};
use tokio::{task::JoinHandle, time::delay_for};
use tower::{builder::ServiceBuilder, retry::Retry, timeout::Timeout, Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::{
    block::{self, Block},
    parameters::Network,
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
const LOOKAHEAD_LIMIT: usize = checkpoint::MAX_CHECKPOINT_HEIGHT_GAP * 2;
/// Controls how long we wait for a block download request to complete.
const BLOCK_TIMEOUT: Duration = Duration::from_secs(6);
/// Controls how long we wait to restart syncing after finishing a sync run.
const SYNC_RESTART_TIMEOUT: Duration = Duration::from_secs(20);

/// Helps work around defects in the bitcoin protocol by checking whether
/// the returned hashes actually extend a chain tip.
#[derive(Debug, Hash, PartialEq, Eq)]
struct CheckedTip {
    tip: block::Hash,
    expected_next: block::Hash,
}

#[derive(Debug)]
pub struct Syncer<ZN, ZS, ZV>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = Error> + Send + Clone + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = Error> + Send + Clone + 'static,
    ZS::Future: Send,
    ZV: Service<Arc<Block>, Response = block::Hash, Error = Error> + Send + Clone + 'static,
    ZV::Future: Send,
{
    /// Used to perform extendtips requests, with no retry logic (failover is handled using fanout).
    tip_network: ZN,
    /// Used to download blocks, with retry logic.
    block_network: Retry<RetryLimit, Timeout<ZN>>,
    state: ZS,
    verifier: ZV,
    prospective_tips: HashSet<CheckedTip>,
    pending_blocks: Pin<Box<FuturesUnordered<JoinHandle<Result<block::Hash, Error>>>>>,
    genesis_hash: block::Hash,
}

impl<ZN, ZS, ZV> Syncer<ZN, ZS, ZV>
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
        let block_network = ServiceBuilder::new()
            .retry(RetryLimit::new(3))
            .timeout(BLOCK_TIMEOUT)
            .service(peers.clone());
        Self {
            tip_network: peers,
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
            // Wipe state from prevous iterations.
            self.prospective_tips = HashSet::new();
            self.pending_blocks = Box::pin(FuturesUnordered::new());

            tracing::info!("starting sync, obtaining new tips");
            if self.obtain_tips().await.is_err() {
                tracing::warn!("failed to obtain tips, waiting to restart sync");
                delay_for(SYNC_RESTART_TIMEOUT).await;
                continue 'sync;
            };
            self.update_metrics();

            while !self.prospective_tips.is_empty() {
                // Check whether any block tasks are currently ready:
                while let Some(Some(rsp)) = self.pending_blocks.next().now_or_never() {
                    match rsp.expect("block download tasks should not panic") {
                        Ok(hash) => tracing::debug!(?hash, "verified and committed block to state"),
                        Err(e) => {
                            tracing::info!(?e, "restarting sync");
                            continue 'sync;
                        }
                    }
                    self.update_metrics();
                }

                // If we have too many pending tasks, wait for one to finish:
                if self.pending_blocks.len() > LOOKAHEAD_LIMIT {
                    tracing::debug!(
                        tips.len = self.prospective_tips.len(),
                        pending.len = self.pending_blocks.len(),
                        pending.limit = LOOKAHEAD_LIMIT,
                        "waiting for pending blocks",
                    );
                    match self
                        .pending_blocks
                        .next()
                        .await
                        .expect("pending_blocks is nonempty")
                        .expect("block download tasks should not panic")
                    {
                        Ok(hash) => tracing::debug!(?hash, "verified and committed block to state"),
                        Err(e) => {
                            tracing::info!(?e, "restarting sync");
                            continue 'sync;
                        }
                    }
                } else {
                    // Otherwise, we can keep extending the tips.
                    tracing::info!(
                        tips.len = self.prospective_tips.len(),
                        pending.len = self.pending_blocks.len(),
                        pending.limit = LOOKAHEAD_LIMIT,
                        "extending tips",
                    );
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

                    if !download_set.contains(&new_tip.expected_next) {
                        tracing::debug!(?new_tip, "adding new prospective tip");
                        self.prospective_tips.insert(new_tip);
                    } else {
                        tracing::debug!(?new_tip, "discarding tip already queued for download");
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

                        let unknown_hashes = match hashes.split_first() {
                            None => continue,
                            Some((expected_hash, rest)) if expected_hash == &tip.expected_next => {
                                rest
                            }
                            Some((other_hash, _rest)) => {
                                tracing::debug!(?other_hash, ?tip.expected_next, "discarding response with unexpected next hash");
                                continue;
                            }
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

                        tracing::trace!(?hashes);

                        if !download_set.contains(&new_tip.expected_next) {
                            tracing::debug!(?new_tip, "adding new prospective tip");
                            self.prospective_tips.insert(new_tip);
                        } else {
                            tracing::debug!(?new_tip, "discarding tip already queued for download");
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
        while !self.state_contains(self.genesis_hash).await? {
            self.request_blocks(vec![self.genesis_hash]).await?;
            match self
                .pending_blocks
                .next()
                .await
                .expect("inserted a download request")
                .expect("block download tasks should not panic")
            {
                Ok(hash) => tracing::debug!(?hash, "verified and committed block to state"),
                Err(e) => tracing::warn!(?e, "could not download genesis block, retrying"),
            }
        }

        Ok(())
    }

    /// Queue downloads for each block that isn't currently known to our node
    async fn request_blocks(&mut self, hashes: Vec<block::Hash>) -> Result<(), Report> {
        tracing::debug!(hashes.len = hashes.len(), "requesting blocks");
        for hash in hashes.into_iter() {
            // TODO: remove this check once the sync service is more reliable
            if self.state_contains(hash).await? {
                tracing::debug!(
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

            tracing::debug!(?hash, "requested block");

            let span = tracing::info_span!("block_fetch_verify", ?hash);
            let mut verifier = self.verifier.clone();
            let task = tokio::spawn(
                async move {
                    let block = match block_req.await {
                        Ok(zn::Response::Blocks(blocks)) => blocks
                            .into_iter()
                            .next()
                            .expect("successful response has the block in it"),
                        Ok(_) => unreachable!("wrong response to block request"),
                        Err(e) => return Err(e),
                    };
                    metrics::counter!("sync.downloaded_blocks", 1);

                    let result = verifier.ready_and().await?.call(block).await;
                    metrics::counter!("sync.verified_blocks", 1);
                    result
                }
                .instrument(span),
            );
            self.pending_blocks.push(task);
        }

        Ok(())
    }

    /// Returns `Ok(true)` if the hash is present in the state, and `Ok(false)`
    /// if the hash is not present in the state.
    ///
    /// Returns `Err(_)` if an error occurs.
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
