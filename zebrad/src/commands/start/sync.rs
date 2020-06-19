#![allow(unused_variables, dead_code)]
use color_eyre::Report;
use eyre::eyre;
use futures::stream::{FuturesUnordered, StreamExt};
use std::{collections::HashSet, iter};
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;
use zebra_chain::{block::BlockHeaderHash, types::BlockHeight};

pub struct Syncer<ZN, ZS>
where
    ZN: Service<zebra_network::Request>,
{
    pub peer_set: ZN,
    pub state: ZS,
    pub tip_requests: FuturesUnordered<ZN::Future>,
    pub block_requests: FuturesUnordered<ZN::Future>,
    pub block_locator: Vec<BlockHeaderHash>,
    pub downloading: HashSet<BlockHeaderHash>,
    pub downloaded: HashSet<BlockHeaderHash>,
    // TODO(jlusby): move to a config
    pub fanout: u32,
}

impl<ZN, ZS> Syncer<ZN, ZS>
where
    ZN: Service<zebra_network::Request, Response = zebra_network::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    ZN::Future: Send,
    ZS: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    ZS::Future: Send,
{
    pub async fn run(&mut self) -> Result<(), Report> {
        loop {
            if self.tip_requests.is_empty() {
                info!("populating prospective chain list");
                self.populate_prospectives(vec![super::GENESIS]).await?;
            }

            info!("extending prospective chains");
            self.extend_chains().await?;
        }
    }
    /// Given a block_locator list fan out request for subsequent hashes to
    /// multiple peers
    pub async fn populate_prospectives(
        &mut self,
        block_locator: Vec<BlockHeaderHash>,
    ) -> Result<(), Report> {
        for _ in 0..self.fanout {
            let req = self.peer_set.ready_and().await.map_err(|e| eyre!(e))?.call(
                zebra_network::Request::FindBlocks {
                    known_blocks: block_locator.clone(),
                    stop: None,
                },
            );
            self.tip_requests.push(req);
        }

        self.block_locator = block_locator;

        Ok(())
    }

    /// Drive all chain extending futures to completion, request unknown blocks,
    /// and extend prospective chain requests
    pub async fn extend_chains(&mut self) -> Result<(), Report> {
        let mut tip_set = HashSet::<BlockHeaderHash>::new();
        while !self.tip_requests.is_empty() {
            match self
                .tip_requests
                .next()
                .await
                .expect("expected: tip_requests is never empty")
                .map_err::<Report, _>(|e| eyre!(e))
            {
                Ok(zebra_network::Response::BlockHeaderHashes(hashes)) => {
                    info!(
                        new_hashes = hashes.len(),
                        in_flight = self.block_requests.len(),
                        downloaded = self.downloaded.len(),
                        "requested more hashes"
                    );
                    let new_tip = hashes[0];
                    let _ = tip_set.insert(new_tip);
                    self.request_blocks(hashes).await?;
                }
                Ok(_) => continue,
                Err(e) => {
                    error!("{:?}", e);
                }
            }
        }

        for tip in tip_set {
            let mut block_locator = self.block_locator.clone();
            block_locator[0] = tip;
            self.populate_prospectives(block_locator).await?;
        }

        Ok(())
    }

    /// Queue downloads for each block that isn't currently known to our node
    pub async fn request_blocks(&mut self, mut hashes: Vec<BlockHeaderHash>) -> Result<(), Report> {
        hashes.retain(|hash| !self.known_block(hash));

        for chunk in hashes.chunks(10usize) {
            self.queue_download(chunk).await?;
        }
        Ok(())
    }

    /// Drive block downloading futures to completion and dispatch downloaded
    /// blocks to the validator
    pub async fn process_blocks(&mut self) -> Result<(), Report> {
        while let Some(res) = self.block_requests.next().await {
            match res.map_err::<Report, _>(|e| eyre!(e)) {
                Ok(zebra_network::Response::Blocks(blocks)) => {
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
            .call(zebra_state::Request::AddBlock { block });

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
            .call(zebra_network::Request::BlocksByHash(set));

        self.downloading.extend(chunk);
        self.block_requests.push(request);

        Ok(())
    }
}

/// Get the heights of the blocks for constructing a block_locator list
pub fn block_locator_heights(tip_height: BlockHeight) -> impl Iterator<Item = BlockHeight> {
    iter::successors(Some(1u32), |h| h.checked_mul(2))
        .flat_map(move |step| tip_height.0.checked_sub(step))
        .map(BlockHeight)
        .chain(iter::once(BlockHeight(0)))
}

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
type NumReq = u32;
