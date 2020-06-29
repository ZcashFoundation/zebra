use color_eyre::eyre::{eyre, Report};
use futures::stream::{FuturesUnordered, StreamExt};
use std::{collections::HashSet, iter, sync::Arc, time::Duration};
use tokio::time::delay_for;
use tower::{Service, ServiceExt};
use zebra_chain::{
    block::{Block, BlockHeaderHash},
    types::BlockHeight,
};

use zebra_network as zn;
use zebra_state as zs;

pub struct Syncer<ZN, ZS, ZV>
where
    ZN: Service<zn::Request>,
{
    pub peer_set: ZN,
    pub state: ZS,
    pub verifier: ZV,
    pub prospective_tips: HashSet<BlockHeaderHash>,
    pub block_requests: FuturesUnordered<ZN::Future>,
    pub fanout: NumReq,
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
                    Ok(zn::Response::BlockHeaderHashes(mut hashes)) => {
                        let new_tip = if let Some(tip) = hashes.pop() {
                            tip
                        } else {
                            tracing::debug!("skipping empty response");
                            continue;
                        };

                        // ExtendTips Step 3
                        //
                        // For each response, check whether the first hash in the
                        // response is the genesis block; if so, discard the response.
                        // It indicates that the remote peer does not have any blocks
                        // following the prospective tip.
                        // TODO(jlusby): reject both main and test net genesis blocks
                        match hashes.first() {
                            Some(&super::GENESIS) => {
                                tracing::debug!("skipping response that does not extend the tip");
                                continue;
                            }
                            Some(_) | None => {}
                        }

                        // ExtendTips Step 4
                        //
                        // Combine the last elements of the remaining responses into
                        // a set, and add this set to the set of prospective tips.
                        let _ = self.prospective_tips.insert(new_tip);

                        download_set.extend(hashes);
                    }
                    Ok(r) => tracing::error!("unexpected response {:?}", r),
                    Err(e) => tracing::error!("{:?}", e),
                }
            }
        }

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
    async fn request_blocks(&mut self, hashes: Vec<BlockHeaderHash>) -> Result<(), Report> {
        for chunk in hashes.chunks(10usize) {
            let set = chunk.iter().cloned().collect();

            let request = self
                .peer_set
                .ready_and()
                .await
                .map_err(|e| eyre!(e))?
                .call(zn::Request::BlocksByHash(set));

            let mut verifier = self.verifier.clone();

            let _ = tokio::spawn(async move {
                match async move {
                    let resp = request.await?;

                    if let zn::Response::Blocks(blocks) = resp {
                        debug!(count = blocks.len(), "received blocks");

                        for block in blocks {
                            let _hash = verifier.ready_and().await?.call(block).await?;
                        }
                    } else {
                        debug!(?resp, "unexpected response");
                    }

                    Ok::<_, Error>(())
                }
                .await
                {
                    Ok(()) => {}
                    Err(e) => {
                        // TODO(jlusby): retry policy?
                        error!("{:?}", e);
                    }
                }
            });
        }

        Ok(())
    }
}

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
