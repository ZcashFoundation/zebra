//! The primary implementation of the `zebra_state::Service` built upon sled
use crate::Config;
use std::{convert::TryInto, future::Future, sync::Arc};
use zebra_chain::serialization::ZcashSerialize;
use zebra_chain::{
    block::{self, Block},
    parameters::Network,
};

use crate::BoxError;

#[derive(Clone)]
pub struct SledState {
    hash_by_height: sled::Tree,
    height_by_hash: sled::Tree,
    block_by_height: sled::Tree,
    // tx_by_hash: sled::Tree,
    // utxo_by_outpoint: sled::Tree,
    // sprout_nullifiers: sled::Tree,
    // sapling_nullifiers: sled::Tree,
    // sprout_anchors: sled::Tree,
    // sapling_anchors: sled::Tree,
}

impl SledState {
    pub fn new(config: &Config, network: Network) -> Self {
        let db = config.sled_config(network).open().unwrap();

        Self {
            hash_by_height: db.open_tree(b"hash_by_height").unwrap(),
            height_by_hash: db.open_tree(b"height_by_hash").unwrap(),
            block_by_height: db.open_tree(b"block_by_height").unwrap(),
            // tx_by_hash: db.open_tree(b"tx_by_hash").unwrap(),
            // utxo_by_outpoint: db.open_tree(b"utxo_by_outpoint").unwrap(),
            // sprout_nullifiers: db.open_tree(b"sprout_nullifiers").unwrap(),
            // sapling_nullifiers: db.open_tree(b"sapling_nullifiers").unwrap(),
        }
    }

    /// Commit a finalized block to the state. It's the caller's responsibility
    /// to ensure that blocks are committed in order.
    pub fn commit_finalized(&self, block: Arc<Block>) -> Result<block::Hash, BoxError> {
        // The only valid block without a coinbase height is the genesis
        // block.  By this point the block has been validated, so if
        // there's no coinbase height, it must be the genesis block.
        let height = block.coinbase_height().unwrap_or(block::Height(0));
        let height_bytes = height.0.to_be_bytes();
        let hash = block.hash();

        use sled::Transactional;
        (
            &self.hash_by_height,
            &self.height_by_hash,
            &self.block_by_height,
        )
            .transaction(|(hash_by_height, height_by_hash, block_by_height)| {
                // TODO: do serialization above
                // for some reason this wouldn't move into the closure (??)
                let block_bytes = block
                    .zcash_serialize_to_vec()
                    .expect("zcash_serialize_to_vec has wrong return type");

                // TODO: check highest entry of hash_by_height as in RFC

                hash_by_height.insert(&height_bytes, &hash.0)?;
                height_by_hash.insert(&hash.0, &height_bytes)?;
                block_by_height.insert(&height_bytes, block_bytes)?;

                // for some reason type inference fails here
                Ok::<_, sled::transaction::ConflictableTransactionError>(())
            })?;

        Ok(hash)
    }

    // TODO: this impl works only during checkpointing, it needs to be rewritten
    pub fn block_locator(&self) -> impl Future<Output = Result<Vec<block::Hash>, BoxError>> {
        let hash_by_height = self.hash_by_height.clone();

        let tip = self.tip();

        async move {
            let (tip_height, _) = match tip.await? {
                Some(height) => height,
                None => return Ok(Vec::new()),
            };

            let heights = crate::util::block_locator_heights(tip_height);
            let mut hashes = Vec::with_capacity(heights.len());
            for height in heights {
                if let Some(bytes) = hash_by_height.get(&height.0.to_be_bytes())? {
                    let hash = block::Hash(bytes.as_ref().try_into().unwrap());
                    hashes.push(hash)
                }
            }
            Ok(hashes)
        }
    }

    pub fn tip(
        &self,
    ) -> impl Future<Output = Result<Option<(block::Height, block::Hash)>, BoxError>> {
        let hash_by_height = self.hash_by_height.clone();
        async move {
            Ok(hash_by_height
                .iter()
                .rev()
                .next()
                .transpose()?
                .map(|(height_bytes, hash_bytes)| {
                    let height = block::Height(u32::from_be_bytes(
                        height_bytes.as_ref().try_into().unwrap(),
                    ));
                    let hash = block::Hash(hash_bytes.as_ref().try_into().unwrap());
                    (height, hash)
                }))
        }
    }

    pub fn depth(&self, hash: block::Hash) -> impl Future<Output = Result<Option<u32>, BoxError>> {
        let height_by_hash = self.height_by_hash.clone();

        // TODO: this impl works only during checkpointing, it needs to be rewritten
        let tip = self.tip();

        async move {
            let height = match height_by_hash.get(&hash.0)? {
                Some(bytes) => {
                    block::Height(u32::from_be_bytes(bytes.as_ref().try_into().unwrap()))
                }
                None => return Ok(None),
            };

            let (tip_height, _) = tip.await?.expect("tip must exist");

            Ok(Some(tip_height.0 - height.0))
        }
    }
}
