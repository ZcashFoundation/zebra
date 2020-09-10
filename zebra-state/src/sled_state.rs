//! The primary implementation of the `zebra_state::Service` built upon sled

use std::{collections::HashMap, convert::TryInto, future::Future};

use zebra_chain::serialization::ZcashSerialize;
use zebra_chain::{
    block::{self},
    parameters::Network,
};

use crate::{BoxError, Config, QueuedBlock};

pub struct SledState {
    /// Queued blocks that arrived out of order, indexed by their parent block hash.
    queued_by_prev_hash: HashMap<block::Hash, QueuedBlock>,

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
            queued_by_prev_hash: HashMap::new(),
            hash_by_height: db.open_tree(b"hash_by_height").unwrap(),
            height_by_hash: db.open_tree(b"height_by_hash").unwrap(),
            block_by_height: db.open_tree(b"block_by_height").unwrap(),
            // tx_by_hash: db.open_tree(b"tx_by_hash").unwrap(),
            // utxo_by_outpoint: db.open_tree(b"utxo_by_outpoint").unwrap(),
            // sprout_nullifiers: db.open_tree(b"sprout_nullifiers").unwrap(),
            // sapling_nullifiers: db.open_tree(b"sapling_nullifiers").unwrap(),
        }
    }

    /// Queue a finalized block to be committed to the state.
    pub fn queue(&mut self, queued_block: QueuedBlock) {
        let prev_hash = queued_block.block.header.previous_block_hash;
        self.queued_by_prev_hash.insert(prev_hash, queued_block);
    }

    pub fn process_queue(&mut self) {
        // Cloning means the closure doesn't hold a borrow of &self,
        // conflicting with mutable access in the loop below.
        let hash_by_height = self.hash_by_height.clone();
        let tip_hash = || {
            read_tip(&hash_by_height)
                .expect("inability to look up tip is unrecoverable")
                .map(|(_height, hash)| hash)
                .unwrap_or(block::Hash([0; 32]))
        };

        while let Some(queued_block) = self.queued_by_prev_hash.remove(&tip_hash()) {
            self.commit_finalized(queued_block)
        }
    }

    /// Commit a finalized block to the state. It's the caller's responsibility
    /// to ensure that blocks are committed in order.
    fn commit_finalized(&mut self, queued_block: QueuedBlock) {
        let QueuedBlock { block, rsp_tx } = queued_block;

        // The only valid block without a coinbase height is the genesis
        // block.  By this point the block has been validated, so if
        // there's no coinbase height, it must be the genesis block.
        let height = block.coinbase_height().unwrap_or(block::Height(0));
        let height_bytes = height.0.to_be_bytes();
        let hash = block.hash();

        use sled::Transactional;
        let transaction_result = (
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
            });

        let _ = rsp_tx.send(transaction_result.map(|_| hash).map_err(Into::into));
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
        async move { read_tip(&hash_by_height) }
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

// Split into a helper function to be called synchronously or asynchronously.
fn read_tip(hash_by_height: &sled::Tree) -> Result<Option<(block::Height, block::Hash)>, BoxError> {
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
