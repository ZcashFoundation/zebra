use std::{collections::HashSet, sync::Arc, time::Duration};

use color_eyre::eyre::{eyre, Report};
use futures::stream::{FuturesUnordered, StreamExt};
use tokio::time::delay_for;
use tower::{retry::Retry, Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::block::{Block, BlockHeaderHash};
use zebra_consensus::checkpoint;
use zebra_network::{self as zn, RetryLimit};
use zebra_state::{self as zs};

pub struct Syncer<ZN, ZS, ZV>
where
    ZN: Service<zn::Request>,
{
    pub peer_set: ZN,
    pub state: ZS,
    pub verifier: ZV,
    pub retry_peer_set: Retry<RetryLimit, ZN>,
    pub prospective_tips: HashSet<BlockHeaderHash>,
    pub block_requests: FuturesUnordered<ZN::Future>,
    pub fanout: NumReq,
}

impl<ZN, ZS, ZC> Syncer<ZN, ZS, ZC>
where
    ZN: Service<zn::Request> + Clone,
{
    pub fn new(peer_set: ZN, state: ZS, verifier: ZC) -> Self {
        let retry_peer_set = Retry::new(RetryLimit::new(3), peer_set.clone());
        Self {
            peer_set,
            state,
            verifier,
            retry_peer_set,
            block_requests: FuturesUnordered::new(),
            // Limit the fanout to the number of chains that the
            // CheckpointVerifier can handle
            fanout: checkpoint::MAX_QUEUED_BLOCKS_PER_HEIGHT,
            prospective_tips: HashSet::new(),
        }
    }
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
    #[instrument(skip(self))]
    pub async fn sync(&mut self) -> Result<(), Report> {
        loop {
            self.obtain_tips().await?;

            // ObtainTips Step 6
            //
            // If there are any prospective tips, call ExtendTips. Continue this step until there are no more prospective tips.
            while !self.prospective_tips.is_empty() {
                info!("extending prospective tips");
                self.extend_tips().await?;
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
        //
        // TODO(jlusby): get the block_locator from the state
        let block_locator = self
            .state
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::GetBlockLocator)
            .await
            .map(|response| match response {
                zebra_state::Response::BlockLocator { block_locator } => block_locator,
                _ => unreachable!(
                    "GetBlockLocator request can only result in Response::BlockLocator"
                ),
            })
            .map_err(|e| eyre!(e))?;
        let mut tip_futs = FuturesUnordered::new();
        tracing::info!(?block_locator, "trying to obtain new chain tips");

        // ObtainTips Step 2
        //
        // Make a FindBlocksByHash request to the network F times, where F is a
        // fanout parameter, to get resp1, ..., respF
        for _ in 0..self.fanout {
            let req = self.peer_set.ready_and().await.map_err(|e| eyre!(e))?.call(
                zn::Request::FindBlocks {
                    known_blocks: block_locator.clone(),
                    stop: None,
                },
            );
            tip_futs.push(req);
        }

        let mut download_set = HashSet::new();
        while let Some(res) = tip_futs.next().await {
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
                        // XXX until we fix the TODO above to construct the locator correctly,
                        // we might hit this case, but it will be unexpected afterwards.
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
                Ok(r) => tracing::info!("unexpected response {:?}", r),
                Err(e) => tracing::info!("{:?}", e),
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
            let mut tip_futs = FuturesUnordered::new();
            for _ in 0..self.fanout {
                tip_futs.push(self.peer_set.ready_and().await.map_err(|e| eyre!(e))?.call(
                    zn::Request::FindBlocks {
                        known_blocks: vec![tip],
                        stop: None,
                    },
                ));
            }
            while let Some(res) = tip_futs.next().await {
                match res.map_err::<Report, _>(|e| eyre!(e)) {
                    Ok(zn::Response::BlockHeaderHashes(mut hashes)) => {
                        // ExtendTips Step 3
                        //
                        // For each response, check whether the first hash in the
                        // response is the genesis block; if so, discard the response.
                        // It indicates that the remote peer does not have any blocks
                        // following the prospective tip.
                        // TODO(jlusby): reject both main and test net genesis blocks
                        match hashes.first() {
                            Some(&super::GENESIS) => {
                                tracing::debug!("skipping response, peer could not extend the tip");
                                continue;
                            }
                            None => {
                                tracing::debug!("skipping empty response");
                                continue;
                            }
                            Some(_) => {}
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
                    Ok(r) => tracing::info!("unexpected response {:?}", r),
                    Err(e) => tracing::info!("{:?}", e),
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

    /// Queue downloads for each block that isn't currently known to our node
    #[instrument(skip(self, hashes))]
    async fn request_blocks(&mut self, hashes: Vec<BlockHeaderHash>) -> Result<(), Report> {
        tracing::debug!(hashes.len = hashes.len(), "requesting blocks");
        for chunk in hashes.chunks(10usize) {
            let set = chunk.iter().cloned().collect();

            let request = self
                .retry_peer_set
                .ready_and()
                .await
                .map_err(|e| eyre!(e))?
                .call(zn::Request::BlocksByHash(set));

            let verifier = self.verifier.clone();

            let _ = tokio::spawn(
                async move {
                    // XXX for some reason the tracing filter
                    // filter = 'info,[sync]=debug'
                    // does not pick this up, even though this future is instrumented
                    // with the current span below.  However, fixing it immediately
                    // isn't critical because this code needs to be changed to propagate
                    // backpressure to the syncer.
                    tracing::debug!("test");
                    let result_fut = async move {
                        let mut handles = FuturesUnordered::new();
                        let resp = request.await?;

                        if let zn::Response::Blocks(blocks) = resp {
                            debug!(count = blocks.len(), "received blocks");

                            for block in blocks {
                                let mut verifier = verifier.clone();
                                let handle = tokio::spawn(async move {
                                    verifier.ready_and().await?.call(block).await
                                });
                                handles.push(handle);
                            }
                        } else {
                            debug!(?resp, "unexpected response");
                        }

                        while let Some(res) = handles.next().await {
                            let _hash = res??;
                        }

                        Ok::<_, Error>(())
                    };

                    match result_fut.await {
                        Ok(()) => {}
                        // Block validation errors are unexpected, they could
                        // be a bug in our code.
                        //
                        // TODO: log request errors at info level
                        Err(e) => warn!("{:?}", e),
                    }
                }
                .instrument(tracing::Span::current()),
            );
        }

        Ok(())
    }
}

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
type NumReq = usize;
