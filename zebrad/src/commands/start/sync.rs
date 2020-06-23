use color_eyre::eyre::{eyre, Report};
use futures::stream::{FuturesUnordered, StreamExt};
use std::{collections::HashSet, iter, time::Duration};
use tokio::time::delay_for;
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;
use zebra_chain::{block::BlockHeaderHash, types::BlockHeight};

use zebra_network as zn;
use zebra_state as zs;

/// Get the heights of the blocks for constructing a block_locator list
#[allow(dead_code)]
pub fn block_locator_heights(tip_height: BlockHeight) -> impl Iterator<Item = BlockHeight> {
    iter::successors(Some(1u32), |h| h.checked_mul(2))
        .flat_map(move |step| tip_height.0.checked_sub(step))
        .map(BlockHeight)
        .chain(iter::once(BlockHeight(0)))
}

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
type NumReq = u32;

pub struct Syncer<ZN, ZS>
where
    ZN: Service<zn::Request>,
{
    pub peer_set: ZN,
    // TODO(jlusby): add validator
    pub state: ZS,
    pub prospective_tips: HashSet<BlockHeaderHash>,
    pub block_requests: FuturesUnordered<ZN::Future>,
    pub downloading: HashSet<BlockHeaderHash>,
    pub downloaded: HashSet<BlockHeaderHash>,
    pub fanout: NumReq,
}

impl<ZN, ZS> Syncer<ZN, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = Error> + Send + Clone + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = Error> + Send + Clone + 'static,
    ZS::Future: Send,
{
    pub async fn run(&mut self) -> Result<(), Report> {
        loop {
            info!("populating prospective tips list");
            self.obtain_tips().await?;

            // ObtainTips Step 6
            //
            // If there are any prospective tips, call ExtendTips. Continue this step until there are no more prospective tips.
            while !self.prospective_tips.is_empty() {
                info!("extending prospective tips");
                self.extend_tips().await?;

                // TODO(jlusby): move this to a background task and check it for errors after each step.
                self.process_blocks().await?;
            }

            delay_for(Duration::from_secs(15)).await;
        }
    }

    /// Given a block_locator list fan out request for subsequent hashes to
    /// multiple peers
    async fn obtain_tips(&mut self) -> Result<(), Report> {
        // ObtainTips Step 1
        //
        // Query the current state to construct the sequence of hashes: handled by
        // the caller
        //
        // TODO(jlusby): get the block_locator from the state
        let block_locator = vec![super::GENESIS];
        let mut tip_futs = FuturesUnordered::new();

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
                    if first_unknown == hashes.len() {
                        tracing::debug!("no new hashes, even though we gave our tip?");
                        continue;
                    }
                    let unknown_hashes = &hashes[first_unknown..];
                    download_set.extend(unknown_hashes);

                    // ObtainTips Step 4
                    //
                    // Combine the last elements of each list into a set; this
                    // is the set of prospective tips.
                    let new_tip = *unknown_hashes
                        .last()
                        .expect("already checked first_unknown < hashes.len()");
                    let _ = self.prospective_tips.insert(new_tip);
                }
                Ok(r) => tracing::error!("unexpected response {:?}", r),
                Err(e) => tracing::error!("{:?}", e),
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

    async fn extend_tips(&mut self) -> Result<(), Report> {
        // Extend Tips 1
        //
        // remove all prospective tips and iterate over them individually
        let tips = std::mem::take(&mut self.prospective_tips);

        let mut download_set = HashSet::new();
        for tip in tips {
            // ExtendTips Step 2
            //
            // Create a FindBlocksByHash request consisting of just the
            // prospective tip. Send this request to the network F times
            for _ in 0..self.fanout {
                let res = self
                    .peer_set
                    .ready_and()
                    .await
                    .map_err(|e| eyre!(e))?
                    .call(zn::Request::FindBlocks {
                        known_blocks: vec![tip],
                        stop: None,
                    })
                    .await;
                match res.map_err::<Report, _>(|e| eyre!(e)) {
                    Ok(zn::Response::BlockHeaderHashes(hashes)) => {
                        if hashes.is_empty() {
                            tracing::debug!("skipping empty response");
                            continue;
                        }

                        // ExtendTips Step 3
                        //
                        // For each response, check whether the first hash in the
                        // response is the genesis block; if so, discard the response.
                        // It indicates that the remote peer does not have any blocks
                        // following the prospective tip.
                        // TODO(jlusby): reject both main and test net genesis blocks
                        if hashes[0] == super::GENESIS {
                            tracing::debug!("skipping response that does not extend the tip");
                            continue;
                        }

                        // ExtendTips Step 4
                        //
                        // Combine the last elements of the remaining responses into
                        // a set, and add this set to the set of prospective tips.
                        let new_tip = *hashes.last().expect("already checked is_empty");
                        let _ = self.prospective_tips.insert(new_tip);

                        // ExtendTips Step 5
                        //
                        // Combine all elements of the remaining responses into a
                        // set, and queue download and verification of those blocks
                        download_set.extend(hashes);
                    }
                    Ok(r) => tracing::error!("unexpected response {:?}", r),
                    Err(e) => tracing::error!("{:?}", e),
                }
            }
        }

        self.request_blocks(download_set.into_iter().collect())
            .await?;

        Ok(())
    }

    /// Queue downloads for each block that isn't currently known to our node
    async fn request_blocks(&mut self, mut hashes: Vec<BlockHeaderHash>) -> Result<(), Report> {
        hashes.retain(|hash| !self.known_block(hash));

        for chunk in hashes.chunks(10usize) {
            self.queue_download(chunk).await?;
        }

        Ok(())
    }

    /// Drive block downloading futures to completion and dispatch downloaded
    /// blocks to the validator
    async fn process_blocks(&mut self) -> Result<(), Report> {
        info!(in_flight = self.block_requests.len(), "processing blocks");

        while let Some(res) = self.block_requests.next().await {
            match res.map_err::<Report, _>(|e| eyre!(e)) {
                Ok(zn::Response::Blocks(blocks)) => {
                    info!(count = blocks.len(), "received blocks");
                    for block in blocks {
                        let hash = block.as_ref().into();
                        assert!(
                            self.downloading.remove(&hash),
                            "all received blocks should be explicitly requested and received once"
                        );
                        let _ = self.downloaded.insert(hash);
                        self.validate_block(block).await?;
                    }
                }
                Ok(_) => continue,
                Err(e) => {
                    error!("{:?}", e);
                }
            }
        }

        Ok(())
    }

    /// Validate a downloaded block using the validator service, inserting the
    /// block into the state if successful
    #[tracing::instrument(skip(self))]
    async fn validate_block(
        &mut self,
        block: std::sync::Arc<zebra_chain::block::Block>,
    ) -> Result<(), Report> {
        let fut = self
            .state
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(zs::Request::AddBlock { block });

        let _handle = tokio::spawn(
            async move {
                match fut.await.map_err::<Report, _>(|e| eyre!(e)) {
                    Ok(_) => {}
                    Err(report) => error!("{:?}", report),
                }
            }
            .in_current_span(),
        );

        Ok(())
    }

    /// Returns true if the block is being downloaded or has been downloaded
    fn known_block(&self, hash: &BlockHeaderHash) -> bool {
        self.downloading.contains(hash) || self.downloaded.contains(hash)
    }

    /// Queue a future to download a set of blocks from the network
    async fn queue_download(&mut self, chunk: &[BlockHeaderHash]) -> Result<(), Report> {
        let set = chunk.iter().cloned().collect();

        let request = self
            .peer_set
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(zn::Request::BlocksByHash(set));

        self.downloading.extend(chunk);
        self.block_requests.push(request);

        Ok(())
    }
}
