//! The primary implementation of the `zebra_state::Service` built upon rocksdb

mod disk_format;
mod iter;

use std::{collections::HashMap, convert::TryInto, sync::Arc};

use zebra_chain::transparent;
use zebra_chain::{
    block::{self, Block},
    parameters::{Network, GENESIS_PREVIOUS_BLOCK_HASH},
    transaction::{self, Transaction},
};

use crate::{service::check, BoxError, Config, FinalizedBlock, HashOrHeight, PreparedBlock};

use self::disk_format::{DiskDeserialize, DiskSerialize, FromDisk, IntoDisk, TransactionLocation};
use self::iter::Iter;

use super::QueuedFinalized;

/// The finalized part of the chain state, stored in the db.
pub struct FinalizedState {
    /// Queued blocks that arrived out of order, indexed by their parent block hash.
    queued_by_prev_hash: HashMap<block::Hash, QueuedFinalized>,
    max_queued_height: i64,

    db: rocksdb::DB,
    ephemeral: bool,
    network: Network,

    /// Commit blocks to the finalized state up to this height, then exit Zebra.
    debug_stop_at_height: Option<block::Height>,
    /// If true, do additional contextual verification checks for debugging.
    debug_contextual_verify: bool,
}

impl FinalizedState {
    pub fn new(config: &Config, network: Network) -> Self {
        let db = config.open_db(network);

        let new_state = Self {
            queued_by_prev_hash: HashMap::new(),
            max_queued_height: -1,
            db,
            ephemeral: config.ephemeral,
            network,
            debug_stop_at_height: config.debug_stop_at_height.map(block::Height),
            debug_contextual_verify: config.debug_contextual_verify,
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

    /// Stop the process if `block_height` is greater than or equal to the
    /// configured stop height.
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
    pub fn queue_and_commit_finalized(&mut self, queued: QueuedFinalized) {
        let prev_hash = queued.0.block.header.previous_block_hash;
        let height = queued.0.height;
        self.queued_by_prev_hash.insert(prev_hash, queued);

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

    fn is_empty(&self, cf: &rocksdb::ColumnFamily) -> bool {
        // use iterator to check if it's empty
        !self
            .db
            .iterator_cf(cf, rocksdb::IteratorMode::Start)
            .valid()
    }

    /// Immediately commit `finalized` to the finalized state.
    pub fn commit_finalized_direct(
        &mut self,
        finalized: FinalizedBlock,
    ) -> Result<block::Hash, BoxError> {
        block_precommit_metrics(&finalized);

        let FinalizedBlock {
            block,
            hash,
            height,
        } = finalized;

        let hash_by_height = self.db.cf_handle("hash_by_height").unwrap();
        let height_by_hash = self.db.cf_handle("height_by_hash").unwrap();
        let block_by_height = self.db.cf_handle("block_by_height").unwrap();
        let tx_by_hash = self.db.cf_handle("tx_by_hash").unwrap();
        let utxo_by_outpoint = self.db.cf_handle("utxo_by_outpoint").unwrap();
        let sprout_nullifiers = self.db.cf_handle("sprout_nullifiers").unwrap();
        let sapling_nullifiers = self.db.cf_handle("sapling_nullifiers").unwrap();

        // Assert that callers (including unit tests) get the chain order correct
        if self.is_empty(hash_by_height) {
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

        // We use a closure so we can use an early return for control flow in
        // the genesis case
        let prepare_commit = || -> rocksdb::WriteBatch {
            let mut batch = rocksdb::WriteBatch::default();

            // Index the block
            batch.zs_insert(hash_by_height, height, hash);
            batch.zs_insert(height_by_hash, hash, height);
            batch.zs_insert(block_by_height, height, &block);

            // TODO: sprout and sapling anchors (per block)

            // Consensus-critical bug in zcashd: transactions in the
            // genesis block are ignored.
            if block.header.previous_block_hash == block::Hash([0; 32]) {
                return batch;
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
                batch.zs_insert(tx_by_hash, transaction_hash, transaction_location);

                // Mark all transparent inputs as spent
                for input in transaction.inputs() {
                    match input {
                        transparent::Input::PrevOut { outpoint, .. } => {
                            batch.delete_cf(utxo_by_outpoint, outpoint.as_bytes());
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
                    batch.zs_insert(utxo_by_outpoint, outpoint, output);
                }

                // Mark sprout and sapling nullifiers as spent
                for sprout_nullifier in transaction.sprout_nullifiers() {
                    batch.zs_insert(sprout_nullifiers, sprout_nullifier, ());
                }
                for sapling_nullifier in transaction.sapling_nullifiers() {
                    batch.zs_insert(sapling_nullifiers, sapling_nullifier, ());
                }
            }

            batch
        };

        let batch = prepare_commit();

        let result = self.db.write(batch).map(|()| hash);

        if result.is_ok() && self.is_at_stop_height(height) {
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
    fn commit_finalized(&mut self, queued_block: QueuedFinalized) {
        let (finalized, rsp_tx) = queued_block;
        self.check_contextual_validity(&finalized);
        let result = self.commit_finalized_direct(finalized);
        let _ = rsp_tx.send(result.map_err(Into::into));
    }

    /// Check that `finalized_block` is contextually valid for the configured network,
    /// based on the committed finalized state.
    ///
    /// Only used for debugging.
    /// Contextual verification is only performed if `debug_contextual_verify` is true,
    /// and there is at least one block in the finalized state.
    ///
    /// Panics if contextual verification fails.
    fn check_contextual_validity(&mut self, finalized_block: &FinalizedBlock) {
        if self.debug_contextual_verify {
            let finalized_tip_height = self.finalized_tip_height();
            // Skip contextual verification if the finalized state is empty
            if finalized_tip_height.is_none() {
                return;
            }

            let relevant_chain = self.chain(finalized_tip_height.map(Into::into));
            check::block_is_contextually_valid(
                // Fake a prepared block
                &PreparedBlock {
                    block: finalized_block.block.clone(),
                    hash: finalized_block.hash,
                    height: finalized_block.height,
                    // TODO: fill in `new_outputs`, or make `block_is_contextually_valid` take a `Block`
                    new_outputs: HashMap::new(),
                },
                self.network,
                finalized_tip_height,
                relevant_chain,
            )
            .expect("block dequeued after CommitFinalized is contextually valid");
        }
    }

    /// Return an iterator over the relevant chain of the block identified by
    /// `hash_or_height`. If `hash_or_height` is `None`, returns an empty
    /// iterator.
    ///
    /// The block identified by `hash_or_height` is included in the chain of
    /// blocks yielded by the iterator.
    ///
    /// Use `.into()` to convert a `block::Hash` or `block::Height` into a
    /// `HashOrHeight`.
    pub fn chain(&self, hash_or_height: Option<HashOrHeight>) -> Iter<'_> {
        use HashOrHeight::*;

        let height = match hash_or_height {
            Some(Hash(hash)) => self.height(hash),
            Some(Height(height)) => Some(height),
            None => None,
        };

        Iter {
            finalized_state: self,
            height,
        }
    }

    /// Returns the tip height and hash if there is one.
    pub fn tip(&self) -> Option<(block::Height, block::Hash)> {
        let hash_by_height = self.db.cf_handle("hash_by_height").unwrap();
        self.db
            .iterator_cf(hash_by_height, rocksdb::IteratorMode::End)
            .next()
            .map(|(height_bytes, hash_bytes)| {
                let height = block::Height::from_bytes(height_bytes);
                let hash = block::Hash::from_bytes(hash_bytes);

                (height, hash)
            })
    }

    /// Returns the height of the given block if it exists.
    pub fn height(&self, hash: block::Hash) -> Option<block::Height> {
        let height_by_hash = self.db.cf_handle("height_by_hash").unwrap();
        self.db.zs_get(&height_by_hash, &hash)
    }

    /// Returns the given block if it exists.
    pub fn block(&self, hash_or_height: HashOrHeight) -> Option<Arc<Block>> {
        let height_by_hash = self.db.cf_handle("height_by_hash").unwrap();
        let block_by_height = self.db.cf_handle("block_by_height").unwrap();
        let height = hash_or_height.height_or_else(|hash| self.db.zs_get(height_by_hash, &hash))?;

        self.db.zs_get(block_by_height, &height)
    }

    /// Returns the `transparent::Output` pointed to by the given
    /// `transparent::OutPoint` if it is present.
    pub fn utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Output> {
        let utxo_by_outpoint = self.db.cf_handle("utxo_by_outpoint").unwrap();
        self.db.zs_get(utxo_by_outpoint, outpoint)
    }

    /// Returns the finalized hash for a given `block::Height` if it is present.
    pub fn hash(&self, height: block::Height) -> Option<block::Hash> {
        let hash_by_height = self.db.cf_handle("hash_by_height").unwrap();
        self.db.zs_get(hash_by_height, &height)
    }

    /// Returns the given transaction if it exists.
    pub fn transaction(&self, hash: transaction::Hash) -> Option<Arc<Transaction>> {
        let tx_by_hash = self.db.cf_handle("tx_by_hash").unwrap();
        self.db
            .zs_get(tx_by_hash, &hash)
            .map(|TransactionLocation { index, height }| {
                let block = self
                    .block(height.into())
                    .expect("block will exist if TransactionLocation does");

                block.transactions[index as usize].clone()
            })
    }
}

// Drop isn't guaranteed to run, such as when we panic, or if someone stored
// their FinalizedState in a static, but it should be fine if we don't clean
// this up since the files are placed in the os temp dir and should be cleaned
// up automatically eventually.
impl Drop for FinalizedState {
    fn drop(&mut self) {
        if self.ephemeral {
            let path = self.db.path();
            tracing::debug!("removing temporary database files {:?}", path);
            let _res = std::fs::remove_dir_all(path);
        }
    }
}

fn block_precommit_metrics(finalized: &FinalizedBlock) {
    let (hash, height, block) = (finalized.hash, finalized.height, finalized.block.as_ref());

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
