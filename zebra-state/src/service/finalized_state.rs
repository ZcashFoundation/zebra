//! The primary implementation of the `zebra_state::Service` built upon rocksdb

mod disk_format;

#[cfg(test)]
mod tests;

use std::{collections::HashMap, convert::TryInto, path::Path, sync::Arc};

use zebra_chain::{
    block::{self, Block},
    orchard,
    parameters::{Network, GENESIS_PREVIOUS_BLOCK_HASH},
    sapling, sprout,
    transaction::{self, Transaction},
    transparent,
};

use crate::{BoxError, Config, FinalizedBlock, HashOrHeight};

use self::disk_format::{DiskDeserialize, DiskSerialize, FromDisk, IntoDisk, TransactionLocation};

use super::QueuedFinalized;

/// The finalized part of the chain state, stored in the db.
pub struct FinalizedState {
    /// Queued blocks that arrived out of order, indexed by their parent block hash.
    queued_by_prev_hash: HashMap<block::Hash, QueuedFinalized>,
    /// A metric tracking the maximum height that's currently in `queued_by_prev_hash`
    ///
    /// Set to `f64::NAN` if `queued_by_prev_hash` is empty, because grafana shows NaNs
    /// as a break in the graph.
    max_queued_height: f64,

    db: rocksdb::DB,
    ephemeral: bool,
    /// Commit blocks to the finalized state up to this height, then exit Zebra.
    debug_stop_at_height: Option<block::Height>,
}

impl FinalizedState {
    pub fn new(config: &Config, network: Network) -> Self {
        let (path, db_options) = config.db_config(network);
        let column_families = vec![
            rocksdb::ColumnFamilyDescriptor::new("hash_by_height", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("height_by_hash", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("block_by_height", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("tx_by_hash", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("utxo_by_outpoint", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("sprout_nullifiers", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("sapling_nullifiers", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("orchard_nullifiers", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("sprout_anchors", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("sapling_anchors", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("orchard_anchors", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("sprout_note_commitment_tree", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new(
                "sapling_note_commitment_tree",
                db_options.clone(),
            ),
            rocksdb::ColumnFamilyDescriptor::new(
                "orchard_note_commitment_tree",
                db_options.clone(),
            ),
        ];
        let db_result = rocksdb::DB::open_cf_descriptors(&db_options, &path, column_families);

        let db = match db_result {
            Ok(d) => {
                tracing::info!("Opened Zebra state cache at {}", path.display());
                d
            }
            // TODO: provide a different hint if the disk is full, see #1623
            Err(e) => panic!(
                "Opening database {:?} failed: {:?}. \
                 Hint: Check if another zebrad process is running. \
                 Try changing the state cache_dir in the Zebra config.",
                path, e,
            ),
        };

        let new_state = Self {
            queued_by_prev_hash: HashMap::new(),
            max_queued_height: f64::NAN,
            db,
            ephemeral: config.ephemeral,
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

                // RocksDB can do a cleanup when column families are opened.
                // So we want to drop it before we exit.
                std::mem::drop(new_state);
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
            self.max_queued_height = f64::NAN;
        } else if self.max_queued_height.is_nan() || self.max_queued_height < height.0 as _ {
            // if there are still blocks in the queue, then either:
            //   - the new block was lower than the old maximum, and there was a gap before it,
            //     so the maximum is still the same (and we skip this code), or
            //   - the new block is higher than the old maximum, and there is at least one gap
            //     between the finalized tip and the new maximum
            self.max_queued_height = height.0 as _;
        }

        metrics::gauge!("state.finalized.queued.max.height", self.max_queued_height);
        metrics::gauge!(
            "state.finalized.queued.block.count",
            self.queued_by_prev_hash.len() as f64
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
    ///
    /// Use `source` as the source of the block in log messages.
    pub fn commit_finalized_direct(
        &mut self,
        finalized: FinalizedBlock,
        source: &str,
    ) -> Result<block::Hash, BoxError> {
        block_precommit_metrics(&finalized);

        let FinalizedBlock {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
        } = finalized;

        let finalized_tip_height = self.finalized_tip_height();

        let hash_by_height = self.db.cf_handle("hash_by_height").unwrap();
        let height_by_hash = self.db.cf_handle("height_by_hash").unwrap();
        let block_by_height = self.db.cf_handle("block_by_height").unwrap();
        let tx_by_hash = self.db.cf_handle("tx_by_hash").unwrap();
        let utxo_by_outpoint = self.db.cf_handle("utxo_by_outpoint").unwrap();

        let sprout_nullifiers = self.db.cf_handle("sprout_nullifiers").unwrap();
        let sapling_nullifiers = self.db.cf_handle("sapling_nullifiers").unwrap();
        let orchard_nullifiers = self.db.cf_handle("orchard_nullifiers").unwrap();

        let sprout_anchors = self.db.cf_handle("sprout_anchors").unwrap();
        let sapling_anchors = self.db.cf_handle("sapling_anchors").unwrap();
        let orchard_anchors = self.db.cf_handle("orchard_anchors").unwrap();

        let sprout_note_commitment_tree_cf =
            self.db.cf_handle("sprout_note_commitment_tree").unwrap();
        let sapling_note_commitment_tree_cf =
            self.db.cf_handle("sapling_note_commitment_tree").unwrap();
        let orchard_note_commitment_tree_cf =
            self.db.cf_handle("orchard_note_commitment_tree").unwrap();

        // Assert that callers (including unit tests) get the chain order correct
        if self.is_empty(hash_by_height) {
            assert_eq!(
                GENESIS_PREVIOUS_BLOCK_HASH, block.header.previous_block_hash,
                "the first block added to an empty state must be a genesis block, source: {}",
                source,
            );
            assert_eq!(
                block::Height(0),
                height,
                "cannot commit genesis: invalid height, source: {}",
                source,
            );
        } else {
            assert_eq!(
                finalized_tip_height.expect("state must have a genesis block committed") + 1,
                Some(height),
                "committed block height must be 1 more than the finalized tip height, source: {}",
                source,
            );

            assert_eq!(
                self.finalized_tip_hash(),
                block.header.previous_block_hash,
                "committed block must be a child of the finalized tip, source: {}",
                source,
            );
        }

        // Read the current note commitment trees. If there are no blocks in the
        // state, these will contain the empty trees.
        let mut sprout_note_commitment_tree = self.sprout_note_commitment_tree();
        let mut sapling_note_commitment_tree = self.sapling_note_commitment_tree();
        let mut orchard_note_commitment_tree = self.orchard_note_commitment_tree();

        // We use a closure so we can use an early return for control flow in
        // the genesis case
        let prepare_commit = || -> rocksdb::WriteBatch {
            let mut batch = rocksdb::WriteBatch::default();

            // Index the block
            batch.zs_insert(hash_by_height, height, hash);
            batch.zs_insert(height_by_hash, hash, height);
            batch.zs_insert(block_by_height, height, &block);

            // "A transaction MUST NOT spend an output of the genesis block coinbase transaction.
            // (There is one such zero-valued output, on each of Testnet and Mainnet .)"
            // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
            if block.header.previous_block_hash == GENESIS_PREVIOUS_BLOCK_HASH {
                // Insert empty note commitment trees. Note that these can't be
                // used too early (e.g. the Orchard tree before Nu5 activates)
                // since the block validation will make sure only appropriate
                // transactions are allowed in a block.
                batch.zs_insert(
                    sprout_note_commitment_tree_cf,
                    height,
                    sprout_note_commitment_tree,
                );
                batch.zs_insert(
                    sapling_note_commitment_tree_cf,
                    height,
                    sapling_note_commitment_tree,
                );
                batch.zs_insert(
                    orchard_note_commitment_tree_cf,
                    height,
                    orchard_note_commitment_tree,
                );
                return batch;
            }

            // Index all new transparent outputs
            for (outpoint, utxo) in new_outputs.into_iter() {
                batch.zs_insert(utxo_by_outpoint, outpoint, utxo);
            }

            // Index each transaction, spent inputs, nullifiers
            for (transaction_index, (transaction, transaction_hash)) in block
                .transactions
                .iter()
                .zip(transaction_hashes.into_iter())
                .enumerate()
            {
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

                // Mark sprout, sapling and orchard nullifiers as spent
                for sprout_nullifier in transaction.sprout_nullifiers() {
                    batch.zs_insert(sprout_nullifiers, sprout_nullifier, ());
                }
                for sapling_nullifier in transaction.sapling_nullifiers() {
                    batch.zs_insert(sapling_nullifiers, sapling_nullifier, ());
                }
                for orchard_nullifier in transaction.orchard_nullifiers() {
                    batch.zs_insert(orchard_nullifiers, orchard_nullifier, ());
                }

                // Add new commitments to the the note commitment trees
                for sprout_note_commitment in transaction.sprout_note_commitments() {
                    sprout_note_commitment_tree.append(sprout_note_commitment);
                }
                for sapling_note_commitment in transaction.sapling_note_commitments() {
                    sapling_note_commitment_tree
                        .append(*sapling_note_commitment)
                        .expect("must work since it was already appended before in the non-finalized state");
                }
                for orchard_note_commitment in transaction.orchard_note_commitments() {
                    orchard_note_commitment_tree
                        .append(*orchard_note_commitment)
                        .expect("must work since it was already appended before in the non-finalized state");
                }
            }

            // Compute the new anchors and index them
            batch.zs_insert(sprout_anchors, height, sprout_note_commitment_tree.root());
            batch.zs_insert(sapling_anchors, height, sapling_note_commitment_tree.root());
            batch.zs_insert(orchard_anchors, height, orchard_note_commitment_tree.root());

            // Update the note commitment trees
            if let Some(h) = finalized_tip_height {
                batch.zs_delete(sprout_note_commitment_tree_cf, h);
                batch.zs_delete(sapling_note_commitment_tree_cf, h);
                batch.zs_delete(orchard_note_commitment_tree_cf, h);
            }
            batch.zs_insert(
                sprout_note_commitment_tree_cf,
                height,
                sprout_note_commitment_tree,
            );
            batch.zs_insert(
                sapling_note_commitment_tree_cf,
                height,
                sapling_note_commitment_tree,
            );
            batch.zs_insert(
                orchard_note_commitment_tree_cf,
                height,
                orchard_note_commitment_tree,
            );

            batch
        };

        let batch = prepare_commit();

        let result = self.db.write(batch).map(|()| hash);

        tracing::trace!(?source, "committed block from");

        if result.is_ok() && self.is_at_stop_height(height) {
            tracing::info!(?source, "committed block from");
            tracing::info!(?height, ?hash, "stopping at configured height");
            // We'd like to drop the database here, because that closes the
            // column families and the database. But Rust's ownership rules
            // make that difficult, so we just flush instead.
            self.db.flush().expect("flush is successful");
            self.delete_ephemeral();
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
        let result = self.commit_finalized_direct(finalized, "CommitFinalized request");
        let _ = rsp_tx.send(result.map_err(Into::into));
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
        self.db.zs_get(height_by_hash, &hash)
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
    pub fn utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Utxo> {
        let utxo_by_outpoint = self.db.cf_handle("utxo_by_outpoint").unwrap();
        self.db.zs_get(utxo_by_outpoint, outpoint)
    }

    /// Returns `true` if the finalized state contains `sprout_nullifier`.
    pub fn contains_sprout_nullifier(&self, sprout_nullifier: &sprout::Nullifier) -> bool {
        let sprout_nullifiers = self.db.cf_handle("sprout_nullifiers").unwrap();
        self.db.zs_contains(sprout_nullifiers, &sprout_nullifier)
    }

    /// Returns `true` if the finalized state contains `sapling_nullifier`.
    pub fn contains_sapling_nullifier(&self, sapling_nullifier: &sapling::Nullifier) -> bool {
        let sapling_nullifiers = self.db.cf_handle("sapling_nullifiers").unwrap();
        self.db.zs_contains(sapling_nullifiers, &sapling_nullifier)
    }

    /// Returns `true` if the finalized state contains `orchard_nullifier`.
    pub fn contains_orchard_nullifier(&self, orchard_nullifier: &orchard::Nullifier) -> bool {
        let orchard_nullifiers = self.db.cf_handle("orchard_nullifiers").unwrap();
        self.db.zs_contains(orchard_nullifiers, &orchard_nullifier)
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

    /// Returns the Sprout note commitment tree of the finalized tip
    /// or the empty tree if the state is empty.
    pub fn sprout_note_commitment_tree(&self) -> sprout::tree::NoteCommitmentTree {
        let height = match self.finalized_tip_height() {
            Some(h) => h,
            None => return Default::default(),
        };
        let sprout_note_commitment_tree = self.db.cf_handle("sprout_note_commitment_tree").unwrap();
        self.db
            .zs_get(sprout_note_commitment_tree, &height)
            .expect("note commitment tree must exist if there is a finalized tip")
    }

    /// Returns the Sapling note commitment tree of the finalized tip
    /// or the empty tree if the state is empty.
    pub fn sapling_note_commitment_tree(&self) -> sapling::tree::NoteCommitmentTree {
        let height = match self.finalized_tip_height() {
            Some(h) => h,
            None => return Default::default(),
        };
        let sapling_note_commitment_tree =
            self.db.cf_handle("sapling_note_commitment_tree").unwrap();
        self.db
            .zs_get(sapling_note_commitment_tree, &height)
            .expect("note commitment tree must exist if there is a finalized tip")
    }

    /// Returns the Orchard note commitment tree of the finalized tip
    /// or the empty tree if the state is empty.
    pub fn orchard_note_commitment_tree(&self) -> orchard::tree::NoteCommitmentTree {
        let height = match self.finalized_tip_height() {
            Some(h) => h,
            None => return Default::default(),
        };
        let orchard_note_commitment_tree =
            self.db.cf_handle("orchard_note_commitment_tree").unwrap();
        self.db
            .zs_get(orchard_note_commitment_tree, &height)
            .expect("note commitment tree must exist if there is a finalized tip")
    }

    /// If the database is `ephemeral`, delete it.
    fn delete_ephemeral(&self) {
        if self.ephemeral {
            let path = self.db.path();
            tracing::debug!("removing temporary database files {:?}", path);
            // We'd like to use `rocksdb::Env::mem_env` for ephemeral databases,
            // but the Zcash blockchain might not fit in memory. So we just
            // delete the database files instead.
            //
            // We'd like to call `DB::destroy` here, but calling destroy on a
            // live DB is undefined behaviour:
            // https://github.com/facebook/rocksdb/wiki/RocksDB-FAQ#basic-readwrite
            //
            // So we assume that all the database files are under `path`, and
            // delete them using standard filesystem APIs. Deleting open files
            // might cause errors on non-Unix platforms, so we ignore the result.
            // (The OS will delete them eventually anyway.)
            let _res = std::fs::remove_dir_all(path);
        }
    }

    /// Returns the `Path` where the files used by this database are located.
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        self.db.path()
    }
}

// Drop isn't guaranteed to run, such as when we panic, or if someone stored
// their FinalizedState in a static, but it should be fine if we don't clean
// this up since the files are placed in the os temp dir and should be cleaned
// up automatically eventually.
impl Drop for FinalizedState {
    fn drop(&mut self) {
        self.delete_ephemeral()
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

    let orchard_nullifier_count = block
        .transactions
        .iter()
        .flat_map(|t| t.orchard_nullifiers())
        .count();

    tracing::debug!(
        ?hash,
        ?height,
        transaction_count,
        transparent_prevout_count,
        transparent_newout_count,
        sprout_nullifier_count,
        sapling_nullifier_count,
        orchard_nullifier_count,
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
    metrics::counter!(
        "state.finalized.cumulative.orchard_nullifiers",
        orchard_nullifier_count as u64
    );
}
