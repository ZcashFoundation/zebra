//! The primary implementation of the `zebra_state::Service` built upon sled

mod disk_format;

use std::{collections::HashMap, convert::TryInto, sync::Arc};

use zebra_chain::transparent;
use zebra_chain::{
    block::{self, Block},
    parameters::{Network, GENESIS_PREVIOUS_BLOCK_HASH},
    transaction::{self, Transaction},
};

use crate::{BoxError, Config, HashOrHeight};

use self::disk_format::{DiskDeserialize, DiskSerialize, FromDisk, IntoDisk, TransactionLocation};

use super::QueuedBlock;

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
    max_queued_height: i64,

    hash_by_height: sled::Tree,
    height_by_hash: sled::Tree,
    block_by_height: sled::Tree,
    tx_by_hash: sled::Tree,
    utxo_by_outpoint: sled::Tree,
    sprout_nullifiers: sled::Tree,
    sapling_nullifiers: sled::Tree,
    // sprout_anchors: sled::Tree,
    // sapling_anchors: sled::Tree,
    /// Commit blocks to the finalized state up to this height, then exit Zebra.
    debug_stop_at_height: Option<block::Height>,
}

impl FinalizedState {
    pub fn new(config: &Config, network: Network) -> Self {
        let db = config.sled_config(network).open().unwrap();

        let new_state = Self {
            queued_by_prev_hash: HashMap::new(),
            max_queued_height: -1,
            hash_by_height: db.open_tree(b"hash_by_height").unwrap(),
            height_by_hash: db.open_tree(b"height_by_hash").unwrap(),
            block_by_height: db.open_tree(b"block_by_height").unwrap(),
            tx_by_hash: db.open_tree(b"tx_by_hash").unwrap(),
            utxo_by_outpoint: db.open_tree(b"utxo_by_outpoint").unwrap(),
            sprout_nullifiers: db.open_tree(b"sprout_nullifiers").unwrap(),
            sapling_nullifiers: db.open_tree(b"sapling_nullifiers").unwrap(),
            debug_stop_at_height: config.debug_stop_at_height.map(block::Height),
        };

        if let Some(tip_height) = new_state.finalized_tip_height() {
            if new_state.is_at_stop_height(tip_height) {
                let debug_stop_at_height = new_state
                    .debug_stop_at_height
                    .expect("true from `is_at_stop_height` implies `debug_stop_at_height` is Some");

                let tip_hash = new_state.finalized_tip_hash();

                if tip_height > debug_stop_at_height {
                    tracing::error!(
                        ?debug_stop_at_height,
                        ?tip_height,
                        ?tip_hash,
                        "previous state height is greater than the stop height",
                    );
                }

                tracing::info!(
                    ?debug_stop_at_height,
                    ?tip_height,
                    ?tip_hash,
                    "state is already at the configured height"
                );

                // There's no need to sync before exit, because the trees have just been opened
                std::process::exit(0);
            }
        }

        new_state
    }

    /// Synchronously flushes all dirty IO buffers and calls fsync.
    ///
    /// Returns the number of bytes flushed during this call.
    /// See sled's `Tree.flush` for more details.
    pub fn flush(&self) -> sled::Result<usize> {
        let mut total_flushed = 0;

        total_flushed += self.hash_by_height.flush()?;
        total_flushed += self.height_by_hash.flush()?;
        total_flushed += self.block_by_height.flush()?;
        total_flushed += self.tx_by_hash.flush()?;
        total_flushed += self.utxo_by_outpoint.flush()?;
        total_flushed += self.sprout_nullifiers.flush()?;
        total_flushed += self.sapling_nullifiers.flush()?;

        Ok(total_flushed)
    }

    /// If `block_height` is greater than or equal to the configured stop height,
    /// stop the process.
    ///
    /// Flushes sled trees before exiting.
    ///
    /// `called_from` and `block_hash` are used for assertions and logging.
    fn is_at_stop_height(&self, block_height: block::Height) -> bool {
        let debug_stop_at_height = match self.debug_stop_at_height {
            Some(debug_stop_at_height) => debug_stop_at_height,
            None => return false,
        };

        if block_height < debug_stop_at_height {
            return false;
        }

        true
    }

    /// Queue a finalized block to be committed to the state.
    ///
    /// After queueing a finalized block, this method checks whether the newly
    /// queued block (and any of its descendants) can be committed to the state.
    pub fn queue_and_commit_finalized_blocks(&mut self, queued_block: QueuedBlock) {
        let prev_hash = queued_block.block.header.previous_block_hash;
        let height = queued_block.block.coinbase_height().unwrap();
        self.queued_by_prev_hash.insert(prev_hash, queued_block);

        while let Some(queued_block) = self.queued_by_prev_hash.remove(&self.finalized_tip_hash()) {
            self.commit_finalized(queued_block);
            metrics::counter!("state.finalized.committed.block.count", 1);
            metrics::gauge!("state.finalized.committed.block.height", height.0 as _);
        }

        if self.queued_by_prev_hash.is_empty() {
            // use -1 as a sentinel value for "None", because 0 is a valid height
            self.max_queued_height = -1;
        } else {
            self.max_queued_height = std::cmp::max(self.max_queued_height, height.0 as _);
        }

        metrics::gauge!("state.finalized.queued.max.height", self.max_queued_height);

        metrics::gauge!(
            "state.finalized.queued.block.count",
            self.queued_by_prev_hash.len() as _
        );
    }

    /// Returns the hash of the current finalized tip block.
    pub fn finalized_tip_hash(&self) -> block::Hash {
        self.tip()
            .map(|(_, hash)| hash)
            // if the state is empty, return the genesis previous block hash
            .unwrap_or(GENESIS_PREVIOUS_BLOCK_HASH)
    }

    /// Returns the height of the current finalized tip block.
    pub fn finalized_tip_height(&self) -> Option<block::Height> {
        self.tip().map(|(height, _)| height)
    }

    /// Immediately commit `block` to the finalized state.
    pub fn commit_finalized_direct(&mut self, block: Arc<Block>) -> Result<block::Hash, BoxError> {
        use sled::Transactional;

        let height = block
            .coinbase_height()
            .expect("finalized blocks are valid and have a coinbase height");
        let hash = block.hash();

        block_precommit_metrics(&hash, height, &block);

        // Assert that callers (including unit tests) get the chain order correct
        if self.block_by_height.is_empty() {
            assert_eq!(
                block::Hash([0; 32]),
                block.header.previous_block_hash,
                "the first block added to an empty state must be a genesis block"
            );
            assert_eq!(
                block::Height(0),
                height,
                "cannot commit genesis: invalid height"
            );
        } else {
            assert_eq!(
                self.finalized_tip_height()
                    .expect("state must have a genesis block committed")
                    + 1,
                Some(height),
                "committed block height must be 1 more than the finalized tip height"
            );

            assert_eq!(
                self.finalized_tip_hash(),
                block.header.previous_block_hash,
                "committed block must be a child of the finalized tip"
            );
        }

        let result = (
            &self.hash_by_height,
            &self.height_by_hash,
            &self.block_by_height,
            &self.utxo_by_outpoint,
            &self.tx_by_hash,
            &self.sprout_nullifiers,
            &self.sapling_nullifiers,
        )
            .transaction(
                move |(
                    hash_by_height,
                    height_by_hash,
                    block_by_height,
                    utxo_by_outpoint,
                    tx_by_hash,
                    sprout_nullifiers,
                    sapling_nullifiers,
                )| {
                    // Index the block
                    hash_by_height.zs_insert(height, hash)?;
                    height_by_hash.zs_insert(hash, height)?;
                    block_by_height.zs_insert(height, &block)?;

                    // TODO: sprout and sapling anchors (per block)

                    // Consensus-critical bug in zcashd: transactions in the
                    // genesis block are ignored.
                    if block.header.previous_block_hash == block::Hash([0; 32]) {
                        return Ok(hash);
                    }

                    // Index each transaction
                    for (transaction_index, transaction) in block.transactions.iter().enumerate() {
                        let transaction_hash = transaction.hash();
                        let transaction_location = TransactionLocation {
                            height,
                            index: transaction_index
                                .try_into()
                                .expect("no more than 4 billion transactions per block"),
                        };
                        tx_by_hash.zs_insert(transaction_hash, transaction_location)?;

                        // Mark all transparent inputs as spent
                        for input in transaction.inputs() {
                            match input {
                                transparent::Input::PrevOut { outpoint, .. } => {
                                    utxo_by_outpoint.remove(outpoint.as_bytes())?;
                                }
                                // Coinbase inputs represent new coins,
                                // so there are no UTXOs to mark as spent.
                                transparent::Input::Coinbase { .. } => {}
                            }
                        }

                        // Index all new transparent outputs
                        for (index, output) in transaction.outputs().iter().enumerate() {
                            let outpoint = transparent::OutPoint {
                                hash: transaction_hash,
                                index: index as _,
                            };
                            utxo_by_outpoint.zs_insert(outpoint, output)?;
                        }

                        // Mark sprout and sapling nullifiers as spent
                        for sprout_nullifier in transaction.sprout_nullifiers() {
                            sprout_nullifiers.zs_insert(sprout_nullifier, ())?;
                        }
                        for sapling_nullifier in transaction.sapling_nullifiers() {
                            sapling_nullifiers.zs_insert(sapling_nullifier, ())?;
                        }
                    }

                    // for some reason type inference fails here
                    Ok::<_, sled::transaction::ConflictableTransactionError>(hash)
                },
            );

        if result.is_ok() && self.is_at_stop_height(height) {
            if let Err(e) = self.flush() {
                tracing::error!(
                    ?e,
                    ?height,
                    ?hash,
                    "error flushing sled state before stopping"
                );
            }

            tracing::info!(?height, ?hash, "stopping at configured height");

            std::process::exit(0);
        }

        result.map_err(Into::into)
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

    /// Returns the tip height and hash if there is one.
    pub fn tip(&self) -> Option<(block::Height, block::Hash)> {
        self.hash_by_height
            .iter()
            .rev()
            .next()
            .transpose()
            .expect("expected that sled errors would not occur")
            .map(|(height_bytes, hash_bytes)| {
                let height = block::Height::from_ivec(height_bytes);
                let hash = block::Hash::from_ivec(hash_bytes);

                (height, hash)
            })
    }

    /// Returns the height of the given block if it exists.
    pub fn height(&self, hash: block::Hash) -> Option<block::Height> {
        self.height_by_hash.zs_get(&hash)
    }

    /// Returns the given block if it exists.
    pub fn block(&self, hash_or_height: HashOrHeight) -> Option<Arc<Block>> {
        let height = hash_or_height.height_or_else(|hash| self.height_by_hash.zs_get(&hash))?;

        self.block_by_height.zs_get(&height)
    }

    /// Returns the `transparent::Output` pointed to by the given
    /// `transparent::OutPoint` if it is present.
    pub fn utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Output> {
        self.utxo_by_outpoint.zs_get(outpoint)
    }

    /// Returns the finalized hash for a given `block::Height` if it is present.
    pub fn hash(&self, height: block::Height) -> Option<block::Hash> {
        self.hash_by_height.zs_get(&height)
    }

    /// Returns the given transaction if it exists.
    pub fn transaction(&self, hash: transaction::Hash) -> Option<Arc<Transaction>> {
        self.tx_by_hash
            .zs_get(&hash)
            .map(|TransactionLocation { index, height }| {
                let block = self
                    .block(height.into())
                    .expect("block will exist if TransactionLocation does");

                block.transactions[index as usize].clone()
            })
    }
}

fn block_precommit_metrics(hash: &block::Hash, height: block::Height, block: &Block) {
    let transaction_count = block.transactions.len();
    let transparent_prevout_count = block
        .transactions
        .iter()
        .flat_map(|t| t.inputs().iter())
        .count()
        // Each block has a single coinbase input which is not a previous output.
        - 1;
    let transparent_newout_count = block
        .transactions
        .iter()
        .flat_map(|t| t.outputs().iter())
        .count();

    let sprout_nullifier_count = block
        .transactions
        .iter()
        .flat_map(|t| t.sprout_nullifiers())
        .count();

    let sapling_nullifier_count = block
        .transactions
        .iter()
        .flat_map(|t| t.sapling_nullifiers())
        .count();

    tracing::debug!(
        ?hash,
        ?height,
        transaction_count,
        transparent_prevout_count,
        transparent_newout_count,
        sprout_nullifier_count,
        sapling_nullifier_count,
        "preparing to commit finalized block"
    );
    metrics::counter!(
        "state.finalized.cumulative.transactions",
        transaction_count as u64
    );
    metrics::counter!(
        "state.finalized.cumulative.transparent_prevouts",
        transparent_prevout_count as u64
    );
    metrics::counter!(
        "state.finalized.cumulative.transparent_newouts",
        transparent_newout_count as u64
    );
    metrics::counter!(
        "state.finalized.cumulative.sprout_nullifiers",
        sprout_nullifier_count as u64
    );
    metrics::counter!(
        "state.finalized.cumulative.sapling_nullifiers",
        sapling_nullifier_count as u64
    );
}
