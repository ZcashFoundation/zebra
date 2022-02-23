//! Provides high-level access to database [`Block`]s and [`Transaction`]s.
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction, and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::{collections::HashMap, sync::Arc};

use zebra_chain::{
    amount::NonNegative,
    block::{self, Block},
    history_tree::HistoryTree,
    parameters::{Network, GENESIS_PREVIOUS_BLOCK_HASH},
    transaction::{self, Transaction},
    transparent,
    value_balance::ValueBalance,
};

use crate::{
    service::finalized_state::{
        disk_db::{DiskDb, DiskWriteBatch, ReadDisk, WriteDisk},
        disk_format::{FromDisk, TransactionLocation},
        zebra_db::shielded::NoteCommitmentTrees,
        FinalizedBlock, FinalizedState,
    },
    BoxError, HashOrHeight,
};

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
}

impl DiskWriteBatch {
    // Write block methods

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
        all_utxos_spent_by_block: HashMap<transparent::OutPoint, transparent::Utxo>,
        // TODO: make an argument struct for all the current note commitment trees & history
        mut note_commitment_trees: NoteCommitmentTrees,
        history_tree: HistoryTree,
        value_pool: ValueBalance<NonNegative>,
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

        // Index the block
        self.zs_insert(hash_by_height, height, hash);
        self.zs_insert(height_by_hash, hash, height);

        // TODO: as part of ticket #3151, commit transaction data, but not UTXOs or address indexes
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
        if self.prepare_genesis_batch(db, &finalized) {
            return Ok(());
        }

        self.prepare_transaction_index_batch(db, &finalized, &mut note_commitment_trees)?;

        self.prepare_note_commitment_batch(
            db,
            &finalized,
            network,
            note_commitment_trees,
            history_tree,
        )?;

        self.prepare_chain_value_pools_batch(db, &finalized, all_utxos_spent_by_block, value_pool)?;

        // The block has passed contextual validation, so update the metrics
        FinalizedState::block_precommit_metrics(block, *hash, *height);

        Ok(())
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
        let FinalizedBlock { block, .. } = finalized;

        if block.header.previous_block_hash == GENESIS_PREVIOUS_BLOCK_HASH {
            self.prepare_genesis_note_commitment_tree_batch(db, finalized);

            return true;
        }

        false
    }

    // Write transaction methods

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
        note_commitment_trees: &mut NoteCommitmentTrees,
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

            DiskWriteBatch::update_note_commitment_trees(transaction, note_commitment_trees)?;
        }

        self.prepare_transparent_outputs_batch(db, finalized)
    }
}
