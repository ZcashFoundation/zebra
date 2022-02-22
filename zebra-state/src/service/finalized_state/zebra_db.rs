//! Provides high-level access to the database using [`zebra_chain`] types.
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction, and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::{borrow::Borrow, collections::HashMap, sync::Arc};

use zebra_chain::{
    amount::NonNegative,
    block::{self, Block, Height},
    history_tree::{HistoryTree, NonEmptyHistoryTree},
    orchard,
    parameters::{Network, GENESIS_PREVIOUS_BLOCK_HASH},
    sapling, sprout,
    transaction::{self, Transaction},
    transparent,
    value_balance::ValueBalance,
};

use crate::{
    service::finalized_state::{
        disk_db::{DiskDb, ReadDisk, WriteDisk},
        disk_format::{FromDisk, TransactionLocation},
        FinalizedBlock, FinalizedState,
    },
    BoxError, HashOrHeight,
};

use super::disk_db::DiskWriteBatch;

impl FinalizedState {
    // Read block methods

    /// Returns true if the database is empty.
    pub fn is_empty(&self) -> bool {
        let hash_by_height = self.db.cf_handle("hash_by_height").unwrap();
        self.db.is_empty(hash_by_height)
    }

    /// Returns the tip height and hash, if there is one.
    pub fn tip(&self) -> Option<(block::Height, block::Hash)> {
        let hash_by_height = self.db.cf_handle("hash_by_height").unwrap();
        self.db
            .reverse_iterator(hash_by_height)
            .next()
            .map(|(height_bytes, hash_bytes)| {
                let height = block::Height::from_bytes(height_bytes);
                let hash = block::Hash::from_bytes(hash_bytes);

                (height, hash)
            })
    }

    /// Returns the finalized hash for a given `block::Height` if it is present.
    pub fn hash(&self, height: block::Height) -> Option<block::Hash> {
        let hash_by_height = self.db.cf_handle("hash_by_height").unwrap();
        self.db.zs_get(hash_by_height, &height)
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

    // Read transaction methods

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

    // Read transparent methods

    /// Returns the `transparent::Output` pointed to by the given
    /// `transparent::OutPoint` if it is present.
    pub fn utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Utxo> {
        let utxo_by_outpoint = self.db.cf_handle("utxo_by_outpoint").unwrap();
        self.db.zs_get(utxo_by_outpoint, outpoint)
    }

    // Read shielded methods

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

    /// Returns `true` if the finalized state contains `sprout_anchor`.
    #[allow(unused)]
    pub fn contains_sprout_anchor(&self, sprout_anchor: &sprout::tree::Root) -> bool {
        let sprout_anchors = self.db.cf_handle("sprout_anchors").unwrap();
        self.db.zs_contains(sprout_anchors, &sprout_anchor)
    }

    /// Returns `true` if the finalized state contains `sapling_anchor`.
    pub fn contains_sapling_anchor(&self, sapling_anchor: &sapling::tree::Root) -> bool {
        let sapling_anchors = self.db.cf_handle("sapling_anchors").unwrap();
        self.db.zs_contains(sapling_anchors, &sapling_anchor)
    }

    /// Returns `true` if the finalized state contains `orchard_anchor`.
    pub fn contains_orchard_anchor(&self, orchard_anchor: &orchard::tree::Root) -> bool {
        let orchard_anchors = self.db.cf_handle("orchard_anchors").unwrap();
        self.db.zs_contains(orchard_anchors, &orchard_anchor)
    }

    // Read chain history methods

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
            .expect("Sprout note commitment tree must exist if there is a finalized tip")
    }

    /// Returns the Sprout note commitment tree matching the given anchor.
    ///
    /// This is used for interstitial tree building, which is unique to Sprout.
    pub fn sprout_note_commitment_tree_by_anchor(
        &self,
        sprout_anchor: &sprout::tree::Root,
    ) -> Option<sprout::tree::NoteCommitmentTree> {
        let sprout_anchors = self.db.cf_handle("sprout_anchors").unwrap();

        self.db.zs_get(sprout_anchors, sprout_anchor)
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
            .expect("Sapling note commitment tree must exist if there is a finalized tip")
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
            .expect("Orchard note commitment tree must exist if there is a finalized tip")
    }

    /// Returns the ZIP-221 history tree of the finalized tip or `None`
    /// if it does not exist yet in the state (pre-Heartwood).
    pub fn history_tree(&self) -> HistoryTree {
        match self.finalized_tip_height() {
            Some(height) => {
                let history_tree_cf = self.db.cf_handle("history_tree").unwrap();
                let history_tree: Option<NonEmptyHistoryTree> =
                    self.db.zs_get(history_tree_cf, &height);
                if let Some(non_empty_tree) = history_tree {
                    HistoryTree::from(non_empty_tree)
                } else {
                    Default::default()
                }
            }
            None => Default::default(),
        }
    }

    /// Returns the stored `ValueBalance` for the best chain at the finalized tip height.
    pub fn finalized_value_pool(&self) -> ValueBalance<NonNegative> {
        let value_pool_cf = self.db.cf_handle("tip_chain_value_pool").unwrap();
        self.db
            .zs_get(value_pool_cf, &())
            .unwrap_or_else(ValueBalance::zero)
    }

    // Update metrics methods - used when writing

    /// Update metrics before committing a block.
    fn block_precommit_metrics(block: &Block, hash: block::Hash, height: block::Height) {
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

        metrics::counter!("state.finalized.block.count", 1);
        metrics::gauge!("state.finalized.block.height", height.0 as _);

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
}

impl DiskWriteBatch {
    /// Prepare a database batch containing `finalized.block`,
    /// and return it (without actually writing anything).
    ///
    /// If this method returns an error, it will be propagated,
    /// and the batch should not be written to the database.
    ///
    /// # Errors
    ///
    /// - Propagates any errors from updating history tree, note commitment trees, or value pools
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_block_batch(
        &mut self,
        db: &DiskDb,
        finalized: FinalizedBlock,
        network: Network,
        current_tip_height: Option<Height>,
        all_utxos_spent_by_block: HashMap<transparent::OutPoint, transparent::Utxo>,
        mut sprout_note_commitment_tree: sprout::tree::NoteCommitmentTree,
        mut sapling_note_commitment_tree: sapling::tree::NoteCommitmentTree,
        mut orchard_note_commitment_tree: orchard::tree::NoteCommitmentTree,
        history_tree: HistoryTree,
        current_value_pool: ValueBalance<NonNegative>,
    ) -> Result<(), BoxError> {
        let hash_by_height = db.cf_handle("hash_by_height").unwrap();
        let height_by_hash = db.cf_handle("height_by_hash").unwrap();
        let block_by_height = db.cf_handle("block_by_height").unwrap();

        let FinalizedBlock {
            block,
            hash,
            height,
            ..
        } = &finalized;

        // The block has passed contextual validation, so update the metrics
        FinalizedState::block_precommit_metrics(block, *hash, *height);

        // Index the block
        self.zs_insert(hash_by_height, height, hash);
        self.zs_insert(height_by_hash, hash, height);
        self.zs_insert(block_by_height, height, block);

        // # Consensus
        //
        // > A transaction MUST NOT spend an output of the genesis block coinbase transaction.
        // > (There is one such zero-valued output, on each of Testnet and Mainnet.)
        //
        // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
        //
        // By returning early, Zebra commits the genesis block and transaction data,
        // but it ignores the genesis UTXO and value pool updates.
        //
        // TODO: commit transaction data but not UTXOs in the next PR.
        if self.prepare_genesis_batch(db, &finalized) {
            return Ok(());
        }

        self.prepare_transaction_index_batch(
            db,
            &finalized,
            &mut sprout_note_commitment_tree,
            &mut sapling_note_commitment_tree,
            &mut orchard_note_commitment_tree,
        )?;

        self.prepare_history_batch(
            db,
            finalized,
            network,
            current_tip_height,
            all_utxos_spent_by_block,
            sprout_note_commitment_tree,
            sapling_note_commitment_tree,
            orchard_note_commitment_tree,
            history_tree,
            current_value_pool,
        )
    }

    /// If `finalized.block` is a genesis block,
    /// prepare a database batch that finishes intializing the database,
    /// and return `true` (without actually writing anything).
    ///
    /// Since the genesis block's transactions are skipped,
    /// the returned genesis batch should be written to the database immediately.
    ///
    /// If `finalized.block` is not a genesis block, does nothing.
    ///
    /// This method never returns an error.
    pub fn prepare_genesis_batch(&mut self, db: &DiskDb, finalized: &FinalizedBlock) -> bool {
        let sprout_note_commitment_tree_cf = db.cf_handle("sprout_note_commitment_tree").unwrap();
        let sapling_note_commitment_tree_cf = db.cf_handle("sapling_note_commitment_tree").unwrap();
        let orchard_note_commitment_tree_cf = db.cf_handle("orchard_note_commitment_tree").unwrap();

        let FinalizedBlock { block, height, .. } = finalized;

        if block.header.previous_block_hash == GENESIS_PREVIOUS_BLOCK_HASH {
            // Insert empty note commitment trees. Note that these can't be
            // used too early (e.g. the Orchard tree before Nu5 activates)
            // since the block validation will make sure only appropriate
            // transactions are allowed in a block.
            self.zs_insert(
                sprout_note_commitment_tree_cf,
                height,
                sprout::tree::NoteCommitmentTree::default(),
            );
            self.zs_insert(
                sapling_note_commitment_tree_cf,
                height,
                sapling::tree::NoteCommitmentTree::default(),
            );
            self.zs_insert(
                orchard_note_commitment_tree_cf,
                height,
                orchard::tree::NoteCommitmentTree::default(),
            );

            return true;
        }

        false
    }

    /// Prepare a database batch containing `finalized.block`'s transaction indexes,
    /// and return it (without actually writing anything).
    ///
    /// If this method returns an error, it will be propagated,
    /// and the batch should not be written to the database.
    ///
    /// # Errors
    ///
    /// - Propagates any errors from updating note commitment trees
    pub fn prepare_transaction_index_batch(
        &mut self,
        db: &DiskDb,
        finalized: &FinalizedBlock,
        sprout_note_commitment_tree: &mut sprout::tree::NoteCommitmentTree,
        sapling_note_commitment_tree: &mut sapling::tree::NoteCommitmentTree,
        orchard_note_commitment_tree: &mut orchard::tree::NoteCommitmentTree,
    ) -> Result<(), BoxError> {
        let tx_by_hash = db.cf_handle("tx_by_hash").unwrap();

        let FinalizedBlock {
            block,
            height,
            transaction_hashes,
            ..
        } = finalized;

        // Index each transaction hash
        for (transaction_index, (transaction, transaction_hash)) in block
            .transactions
            .iter()
            .zip(transaction_hashes.iter())
            .enumerate()
        {
            let transaction_location = TransactionLocation {
                height: *height,
                index: transaction_index
                    .try_into()
                    .expect("no more than 4 billion transactions per block"),
            };
            self.zs_insert(tx_by_hash, transaction_hash, transaction_location);

            self.prepare_nullifier_batch(db, transaction)?;

            DiskWriteBatch::update_note_commitment_trees(
                transaction,
                sprout_note_commitment_tree,
                sapling_note_commitment_tree,
                orchard_note_commitment_tree,
            )?;
        }

        self.prepare_transparent_outputs_batch(db, finalized)
    }

    /// Prepare a database batch containing `finalized.block`'s UTXO changes,
    /// and return it (without actually writing anything).
    ///
    /// # Errors
    ///
    /// - This method doesn't currently return any errors, but it might in future
    pub fn prepare_transparent_outputs_batch(
        &mut self,
        db: &DiskDb,
        finalized: &FinalizedBlock,
    ) -> Result<(), BoxError> {
        let utxo_by_outpoint = db.cf_handle("utxo_by_outpoint").unwrap();

        let FinalizedBlock {
            block, new_outputs, ..
        } = finalized;

        // Index all new transparent outputs, before deleting any we've spent
        for (outpoint, utxo) in new_outputs.borrow().iter() {
            self.zs_insert(utxo_by_outpoint, outpoint, utxo);
        }

        // Mark all transparent inputs as spent.
        //
        // Coinbase inputs represent new coins,
        // so there are no UTXOs to mark as spent.
        for outpoint in block
            .transactions
            .iter()
            .flat_map(|tx| tx.inputs())
            .flat_map(|input| input.outpoint())
        {
            self.zs_delete(utxo_by_outpoint, outpoint);
        }

        Ok(())
    }

    /// Prepare a database batch containing `finalized.block`'s nullifiers,
    /// and return it (without actually writing anything).
    ///
    /// # Errors
    ///
    /// - This method doesn't currently return any errors, but it might in future
    pub fn prepare_nullifier_batch(
        &mut self,
        db: &DiskDb,
        transaction: &Transaction,
    ) -> Result<(), BoxError> {
        let sprout_nullifiers = db.cf_handle("sprout_nullifiers").unwrap();
        let sapling_nullifiers = db.cf_handle("sapling_nullifiers").unwrap();
        let orchard_nullifiers = db.cf_handle("orchard_nullifiers").unwrap();

        // Mark sprout, sapling and orchard nullifiers as spent
        for sprout_nullifier in transaction.sprout_nullifiers() {
            self.zs_insert(sprout_nullifiers, sprout_nullifier, ());
        }
        for sapling_nullifier in transaction.sapling_nullifiers() {
            self.zs_insert(sapling_nullifiers, sapling_nullifier, ());
        }
        for orchard_nullifier in transaction.orchard_nullifiers() {
            self.zs_insert(orchard_nullifiers, orchard_nullifier, ());
        }

        Ok(())
    }

    /// Updates the supplied note commitment trees.
    ///
    /// If this method returns an error, it will be propagated,
    /// and the batch should not be written to the database.
    ///
    /// # Errors
    ///
    /// - Propagates any errors from updating note commitment trees
    pub fn update_note_commitment_trees(
        transaction: &Transaction,
        sprout_note_commitment_tree: &mut sprout::tree::NoteCommitmentTree,
        sapling_note_commitment_tree: &mut sapling::tree::NoteCommitmentTree,
        orchard_note_commitment_tree: &mut orchard::tree::NoteCommitmentTree,
    ) -> Result<(), BoxError> {
        // Update the note commitment trees
        for sprout_note_commitment in transaction.sprout_note_commitments() {
            sprout_note_commitment_tree.append(*sprout_note_commitment)?;
        }
        for sapling_note_commitment in transaction.sapling_note_commitments() {
            sapling_note_commitment_tree.append(*sapling_note_commitment)?;
        }
        for orchard_note_commitment in transaction.orchard_note_commitments() {
            orchard_note_commitment_tree.append(*orchard_note_commitment)?;
        }

        Ok(())
    }

    /// Prepare a database batch containing whole-chain updates from `finalized.block`,
    /// and return it (without actually writing anything).
    ///
    /// Includes the following updates:
    /// - shielded note commitment trees
    /// - chain history tree
    /// - chain value pools
    ///
    /// If this method returns an error, it will be propagated,
    /// and the batch should not be written to the database.
    ///
    /// # Errors
    ///
    /// - Propagates any errors from updating history tree or value pools
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_history_batch(
        &mut self,
        db: &DiskDb,
        finalized: FinalizedBlock,
        network: Network,
        current_tip_height: Option<Height>,
        mut all_utxos_spent_by_block: HashMap<transparent::OutPoint, transparent::Utxo>,
        sprout_note_commitment_tree: sprout::tree::NoteCommitmentTree,
        sapling_note_commitment_tree: sapling::tree::NoteCommitmentTree,
        orchard_note_commitment_tree: orchard::tree::NoteCommitmentTree,
        mut history_tree: HistoryTree,
        current_value_pool: ValueBalance<NonNegative>,
    ) -> Result<(), BoxError> {
        let sprout_anchors = db.cf_handle("sprout_anchors").unwrap();
        let sapling_anchors = db.cf_handle("sapling_anchors").unwrap();
        let orchard_anchors = db.cf_handle("orchard_anchors").unwrap();

        let sprout_note_commitment_tree_cf = db.cf_handle("sprout_note_commitment_tree").unwrap();
        let sapling_note_commitment_tree_cf = db.cf_handle("sapling_note_commitment_tree").unwrap();
        let orchard_note_commitment_tree_cf = db.cf_handle("orchard_note_commitment_tree").unwrap();
        let history_tree_cf = db.cf_handle("history_tree").unwrap();

        let tip_chain_value_pool = db.cf_handle("tip_chain_value_pool").unwrap();

        let FinalizedBlock {
            block,
            height,
            new_outputs,
            ..
        } = finalized;

        let sprout_root = sprout_note_commitment_tree.root();
        let sapling_root = sapling_note_commitment_tree.root();
        let orchard_root = orchard_note_commitment_tree.root();

        history_tree.push(network, block.clone(), sapling_root, orchard_root)?;

        // Compute the new anchors and index them
        // Note: if the root hasn't changed, we write the same value again.
        self.zs_insert(sprout_anchors, sprout_root, &sprout_note_commitment_tree);
        self.zs_insert(sapling_anchors, sapling_root, ());
        self.zs_insert(orchard_anchors, orchard_root, ());

        // Update the trees in state
        if let Some(h) = current_tip_height {
            self.zs_delete(sprout_note_commitment_tree_cf, h);
            self.zs_delete(sapling_note_commitment_tree_cf, h);
            self.zs_delete(orchard_note_commitment_tree_cf, h);
            self.zs_delete(history_tree_cf, h);
        }

        self.zs_insert(
            sprout_note_commitment_tree_cf,
            height,
            sprout_note_commitment_tree,
        );

        self.zs_insert(
            sapling_note_commitment_tree_cf,
            height,
            sapling_note_commitment_tree,
        );

        self.zs_insert(
            orchard_note_commitment_tree_cf,
            height,
            orchard_note_commitment_tree,
        );

        if let Some(history_tree) = history_tree.as_ref() {
            self.zs_insert(history_tree_cf, height, history_tree);
        }

        // Some utxos are spent in the same block so they will be in `new_outputs`.
        all_utxos_spent_by_block.extend(new_outputs);

        let new_pool = current_value_pool.add_block(block.borrow(), &all_utxos_spent_by_block)?;
        self.zs_insert(tip_chain_value_pool, (), new_pool);

        Ok(())
    }
}
