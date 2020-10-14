//! The primary implementation of the `zebra_state::Service` built upon sled

use std::{collections::HashMap, convert::TryInto, future::Future, sync::Arc};

use tracing::trace;
use zebra_chain::{
    block::{self, Block},
    parameters::{Network, GENESIS_PREVIOUS_BLOCK_HASH},
};
use zebra_chain::{
    serialization::{ZcashDeserialize, ZcashSerialize},
    transparent,
};

use crate::{BoxError, Config, HashOrHeight, QueuedBlock};

/// The finalized part of the chain state, stored in sled.
///
/// This structure has two categories of methods:
///
/// - *synchronous* methods that perform writes to the sled state;
/// - *asynchronous* methods that perform reads.
///
/// For more on this distinction, see RFC5. The synchronous methods are
/// implemented as ordinary methods on the [`FinalizedState`]. The asynchronous
/// methods are not implemented using `async fn`, but using normal methods that
/// return `impl Future<Output = ...>`. This allows them to move data (e.g.,
/// clones of handles for [`sled::Tree`]s) into the futures they return.
///
/// This means that the returned futures have a `'static` lifetime and don't
/// borrow any resources from the [`FinalizedState`], and the actual database work is
/// performed asynchronously when the returned future is polled, not while it is
/// created.  This is analogous to the way [`tower::Service::call`] works.
pub struct FinalizedState {
    /// Queued blocks that arrived out of order, indexed by their parent block hash.
    queued_by_prev_hash: HashMap<block::Hash, QueuedBlock>,

    hash_by_height: sled::Tree,
    height_by_hash: sled::Tree,
    block_by_height: sled::Tree,
    // tx_by_hash: sled::Tree,
    utxo_by_outpoint: sled::Tree,
    // sprout_nullifiers: sled::Tree,
    // sapling_nullifiers: sled::Tree,
    // sprout_anchors: sled::Tree,
    // sapling_anchors: sled::Tree,
}

/// Helper trait for inserting (Key, Value) pairs into sled when both the key and
/// value implement ZcashSerialize.
trait SledSerialize {
    /// Serialize and insert the given key and value into a sled tree.
    fn zs_insert<K, V>(
        &self,
        key: &K,
        value: &V,
    ) -> Result<(), sled::transaction::UnabortableTransactionError>
    where
        K: ZcashSerialize,
        V: ZcashSerialize;
}

/// Helper trait for retrieving values from sled trees when the key and value
/// implement ZcashSerialize/ZcashDeserialize.
trait SledDeserialize {
    /// Serialize the given key and use that to get and deserialize the
    /// corresponding value from a sled tree, if it is present.
    fn zs_get<K, V>(&self, key: &K) -> Result<Option<V>, BoxError>
    where
        K: ZcashSerialize,
        V: ZcashDeserialize;
}

impl SledSerialize for sled::transaction::TransactionalTree {
    fn zs_insert<K, V>(
        &self,
        key: &K,
        value: &V,
    ) -> Result<(), sled::transaction::UnabortableTransactionError>
    where
        K: ZcashSerialize,
        V: ZcashSerialize,
    {
        let key_bytes = key
            .zcash_serialize_to_vec()
            .expect("serializing into a vec won't fail");

        let value_bytes = value
            .zcash_serialize_to_vec()
            .expect("serializing into a vec won't fail");

        self.insert(key_bytes, value_bytes)?;

        Ok(())
    }
}

impl SledDeserialize for sled::Tree {
    fn zs_get<K, V>(&self, key: &K) -> Result<Option<V>, BoxError>
    where
        K: ZcashSerialize,
        V: ZcashDeserialize,
    {
        let key_bytes = key
            .zcash_serialize_to_vec()
            .expect("serializing into a vec won't fail");

        let value_bytes = self.get(&key_bytes)?;

        let value = value_bytes
            .as_deref()
            .map(ZcashDeserialize::zcash_deserialize)
            .transpose()?;

        Ok(value)
    }
}

impl FinalizedState {
    pub fn new(config: &Config, network: Network) -> Self {
        let db = config.sled_config(network).open().unwrap();

        Self {
            queued_by_prev_hash: HashMap::new(),
            hash_by_height: db.open_tree(b"hash_by_height").unwrap(),
            height_by_hash: db.open_tree(b"height_by_hash").unwrap(),
            block_by_height: db.open_tree(b"block_by_height").unwrap(),
            // tx_by_hash: db.open_tree(b"tx_by_hash").unwrap(),
            utxo_by_outpoint: db.open_tree(b"utxo_by_outpoint").unwrap(),
            // sprout_nullifiers: db.open_tree(b"sprout_nullifiers").unwrap(),
            // sapling_nullifiers: db.open_tree(b"sapling_nullifiers").unwrap(),
        }
    }

    /// Queue a finalized block to be committed to the state.
    ///
    /// After queueing a finalized block, this method checks whether the newly
    /// queued block (and any of its descendants) can be committed to the state.
    pub fn queue_and_commit_finalized_blocks(&mut self, queued_block: QueuedBlock) {
        let prev_hash = queued_block.block.header.previous_block_hash;
        self.queued_by_prev_hash.insert(prev_hash, queued_block);

        while let Some(queued_block) = self.queued_by_prev_hash.remove(&self.finalized_tip_hash()) {
            let height = queued_block
                .block
                .coinbase_height()
                .expect("valid blocks must have a height");
            self.commit_finalized(queued_block);
            metrics::counter!("state.committed.block.count", 1);
            metrics::gauge!("state.committed.block.height", height.0 as _);
        }

        metrics::gauge!(
            "state.queued.block.count",
            self.queued_by_prev_hash.len() as _
        );
    }

    /// Returns the hash of the current finalized tip block.
    pub fn finalized_tip_hash(&self) -> block::Hash {
        read_tip(&self.hash_by_height)
            .expect("inability to look up tip is unrecoverable")
            .map(|(_, hash)| hash)
            // if the state is empty, return the genesis previous block hash
            .unwrap_or(GENESIS_PREVIOUS_BLOCK_HASH)
    }

    /// Returns the height of the current finalized tip block.
    pub fn finalized_tip_height(&self) -> Option<block::Height> {
        read_tip(&self.hash_by_height)
            .expect("inability to look up tip is unrecoverable")
            .map(|(height, _)| height)
    }

    /// Immediately commit `block` to the finalized state.
    pub fn commit_finalized_direct(&mut self, block: Arc<Block>) -> Result<block::Hash, BoxError> {
        use sled::Transactional;

        let height = block
            .coinbase_height()
            .expect("finalized blocks are valid and have a coinbase height");
        let height_bytes = height.0.to_be_bytes();
        let hash = block.hash();

        trace!(?height, "Finalized block");

        (
            &self.hash_by_height,
            &self.height_by_hash,
            &self.block_by_height,
            &self.utxo_by_outpoint,
        )
            .transaction(
                move |(hash_by_height, height_by_hash, block_by_height, utxo_by_outpoint)| {
                    // TODO: do serialization above
                    // for some reason this wouldn't move into the closure (??)
                    let block_bytes = block
                        .zcash_serialize_to_vec()
                        .expect("zcash_serialize_to_vec has wrong return type");

                    // TODO: check highest entry of hash_by_height as in RFC

                    hash_by_height.insert(&height_bytes, &hash.0)?;
                    height_by_hash.insert(&hash.0, &height_bytes)?;
                    block_by_height.insert(&height_bytes, block_bytes)?;
                    // tx_by_hash

                    for transaction in block.transactions.iter() {
                        let transaction_hash = transaction.hash();
                        for (index, output) in transaction.outputs().iter().enumerate() {
                            let outpoint = transparent::OutPoint {
                                hash: transaction_hash,
                                index: index as _,
                            };

                            utxo_by_outpoint.zs_insert(&outpoint, output)?;
                        }
                    }
                    // sprout_nullifiers
                    // sapling_nullifiers

                    // for some reason type inference fails here
                    Ok::<_, sled::transaction::ConflictableTransactionError>(hash)
                },
            )
            .map_err(Into::into)
    }

    /// Commit a finalized block to the state.
    ///
    /// It's the caller's responsibility to ensure that blocks are committed in
    /// order. This function is called by [`queue`], which ensures order.
    /// It is intentionally not exposed as part of the public API of the
    /// [`FinalizedState`].
    fn commit_finalized(&mut self, queued_block: QueuedBlock) {
        let QueuedBlock { block, rsp_tx } = queued_block;
        let result = self.commit_finalized_direct(block);
        let _ = rsp_tx.send(result.map_err(Into::into));
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

    pub fn block(
        &self,
        hash_or_height: HashOrHeight,
    ) -> impl Future<Output = Result<Option<Arc<Block>>, BoxError>> {
        let height_by_hash = self.height_by_hash.clone();
        let block_by_height = self.block_by_height.clone();

        async move {
            let height = match hash_or_height {
                HashOrHeight::Height(height) => height,
                HashOrHeight::Hash(hash) => match height_by_hash.get(&hash.0)? {
                    Some(bytes) => {
                        block::Height(u32::from_be_bytes(bytes.as_ref().try_into().unwrap()))
                    }
                    None => return Ok(None),
                },
            };

            match block_by_height.get(&height.0.to_be_bytes())? {
                Some(bytes) => Ok(Some(Arc::<Block>::zcash_deserialize(bytes.as_ref())?)),
                None => Ok(None),
            }
        }
    }

    /// Returns the `transparent::Output` pointed to by the given
    /// `transparent::OutPoint` if it is present.
    pub fn utxo(
        &self,
        outpoint: &transparent::OutPoint,
    ) -> Result<Option<transparent::Output>, BoxError> {
        self.utxo_by_outpoint.zs_get(outpoint)
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
