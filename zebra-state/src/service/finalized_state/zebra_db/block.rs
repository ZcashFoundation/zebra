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
    serialization::TrustedPreallocate,
    transaction::{self, Transaction},
    transparent,
    value_balance::ValueBalance,
};

use crate::{
    service::finalized_state::{
        disk_db::{DiskDb, DiskWriteBatch, ReadDisk, WriteDisk},
        disk_format::{FromDisk, TransactionLocation},
        zebra_db::{metrics::block_precommit_metrics, shielded::NoteCommitmentTrees, ZebraDb},
        FinalizedBlock,
    },
    BoxError, HashOrHeight,
};

#[cfg(test)]
mod tests;

impl ZebraDb {
    // Read block methods

    /// Returns true if the database is empty.
    //
    // TODO: move this method to the tip section
    pub fn is_empty(&self) -> bool {
        let hash_by_height = self.db.cf_handle("hash_by_height").unwrap();
        self.db.zs_is_empty(&hash_by_height)
    }

    /// Returns the tip height and hash, if there is one.
    //
    // TODO: move this method to the tip section
    pub fn tip(&self) -> Option<(block::Height, block::Hash)> {
        let hash_by_height = self.db.cf_handle("hash_by_height").unwrap();
        self.db
            .zs_last_key_value(&hash_by_height)
            .map(|(height_bytes, hash_bytes)| {
                let height = block::Height::from_bytes(height_bytes);
                let hash = block::Hash::from_bytes(hash_bytes);

                (height, hash)
            })
    }

    /// Returns the finalized hash for a given `block::Height` if it is present.
    pub fn hash(&self, height: block::Height) -> Option<block::Hash> {
        let hash_by_height = self.db.cf_handle("hash_by_height").unwrap();
        self.db.zs_get(&hash_by_height, &height)
    }

    /// Returns the height of the given block if it exists.
    pub fn height(&self, hash: block::Hash) -> Option<block::Height> {
        let height_by_hash = self.db.cf_handle("height_by_hash").unwrap();
        self.db.zs_get(&height_by_hash, &hash)
    }

    /// Returns the [`Block`] with [`block::Hash`](zebra_chain::block::Hash) or
    /// [`Height`](zebra_chain::block::Height), if it exists in the finalized chain.
    //
    // TODO: move this method to the start of the section
    pub fn block(&self, hash_or_height: HashOrHeight) -> Option<Arc<Block>> {
        // Blocks
        let block_header_by_height = self.db.cf_handle("block_by_height").unwrap();
        let height_by_hash = self.db.cf_handle("height_by_hash").unwrap();

        let height =
            hash_or_height.height_or_else(|hash| self.db.zs_get(&height_by_hash, &hash))?;
        let header = self.db.zs_get(&block_header_by_height, &height)?;

        // Transactions
        let tx_by_loc = self.db.cf_handle("tx_by_loc").unwrap();

        // Fetch the entire block's transactions
        let mut transactions = Vec::new();

        // TODO: is this loop more efficient if we store the number of transactions?
        //       is the difference large enough to matter?
        for tx_index in 0..=Transaction::max_allocation() {
            let tx_loc = TransactionLocation::from_u64(height, tx_index);

            if let Some(tx) = self.db.zs_get(&tx_by_loc, &tx_loc) {
                transactions.push(tx);
            } else {
                break;
            }
        }

        Some(Arc::new(Block {
            header,
            transactions,
        }))
    }

    // Read tip block methods

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

    /// Returns the tip block, if there is one.
    pub fn tip_block(&self) -> Option<Arc<Block>> {
        let (height, _hash) = self.tip()?;
        self.block(height.into())
    }

    // Read transaction methods

    /// Returns the [`TransactionLocation`] for [`transaction::Hash`],
    /// if it exists in the finalized chain.
    pub fn transaction_location(&self, hash: transaction::Hash) -> Option<TransactionLocation> {
        let tx_loc_by_hash = self.db.cf_handle("tx_by_hash").unwrap();
        self.db.zs_get(&tx_loc_by_hash, &hash)
    }

    /// Returns the [`transaction::Hash`] for [`TransactionLocation`],
    /// if it exists in the finalized chain.
    #[allow(dead_code)]
    pub fn transaction_hash(&self, location: TransactionLocation) -> Option<transaction::Hash> {
        let hash_by_tx_loc = self.db.cf_handle("hash_by_tx_loc").unwrap();
        self.db.zs_get(&hash_by_tx_loc, &location)
    }

    /// Returns the [`Transaction`] with [`transaction::Hash`], and its [`block::Height`],
    /// if a transaction with that hash exists in the finalized chain.
    //
    // TODO: move this method to the start of the section
    pub fn transaction(
        &self,
        hash: transaction::Hash,
    ) -> Option<(Arc<Transaction>, block::Height)> {
        let tx_by_loc = self.db.cf_handle("tx_by_loc").unwrap();

        let transaction_location = self.transaction_location(hash)?;

        self.db
            .zs_get(&tx_by_loc, &transaction_location)
            .map(|tx| (tx, transaction_location.height))
    }

    // Write block methods

    /// Write `finalized` to the finalized state.
    ///
    /// Uses:
    /// - `history_tree`: the current tip's history tree
    /// - `network`: the configured network
    /// - `source`: the source of the block in log messages
    ///
    /// # Errors
    ///
    /// - Propagates any errors from writing to the DB
    /// - Propagates any errors from updating history and note commitment trees
    pub(in super::super) fn write_block(
        &mut self,
        finalized: FinalizedBlock,
        history_tree: HistoryTree,
        network: Network,
        source: &str,
    ) -> Result<block::Hash, BoxError> {
        let finalized_hash = finalized.hash;

        // Get a list of the spent UTXOs, before we delete any from the database
        let all_utxos_spent_by_block = finalized
            .block
            .transactions
            .iter()
            .flat_map(|tx| tx.inputs().iter())
            .flat_map(|input| input.outpoint())
            .flat_map(|outpoint| self.utxo(&outpoint).map(|utxo| (outpoint, utxo)))
            .collect();

        let mut batch = DiskWriteBatch::new();

        // In case of errors, propagate and do not write the batch.
        batch.prepare_block_batch(
            &self.db,
            finalized,
            network,
            all_utxos_spent_by_block,
            self.note_commitment_trees(),
            history_tree,
            self.finalized_value_pool(),
        )?;

        self.db.write(batch)?;

        tracing::trace!(?source, "committed block from");

        Ok(finalized_hash)
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
        let FinalizedBlock {
            block,
            hash,
            height,
            ..
        } = &finalized;

        // Commit block and transaction data.
        // (Transaction indexes, note commitments, and UTXOs are committed later.)
        self.prepare_block_header_transactions_batch(db, &finalized)?;

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

        // Commit transaction indexes
        self.prepare_transaction_index_batch(db, &finalized, &mut note_commitment_trees)?;

        self.prepare_note_commitment_batch(
            db,
            &finalized,
            network,
            note_commitment_trees,
            history_tree,
        )?;

        // Commit UTXOs and value pools
        self.prepare_chain_value_pools_batch(db, &finalized, all_utxos_spent_by_block, value_pool)?;

        // The block has passed contextual validation, so update the metrics
        block_precommit_metrics(block, *hash, *height);

        Ok(())
    }

    /// Prepare a database batch containing the block header and transactions
    /// from `finalized.block`, and return it (without actually writing anything).
    ///
    /// # Errors
    ///
    /// - This method does not currently return any errors.
    pub fn prepare_block_header_transactions_batch(
        &mut self,
        db: &DiskDb,
        finalized: &FinalizedBlock,
    ) -> Result<(), BoxError> {
        // Blocks
        let block_header_by_height = db.cf_handle("block_by_height").unwrap();
        let hash_by_height = db.cf_handle("hash_by_height").unwrap();
        let height_by_hash = db.cf_handle("height_by_hash").unwrap();

        // Transactions
        let tx_by_loc = db.cf_handle("tx_by_loc").unwrap();
        let hash_by_tx_loc = db.cf_handle("hash_by_tx_loc").unwrap();
        let tx_loc_by_hash = db.cf_handle("tx_by_hash").unwrap();

        let FinalizedBlock {
            block,
            hash,
            height,
            transaction_hashes,
            ..
        } = finalized;

        // Commit block header data
        self.zs_insert(&block_header_by_height, height, block.header);

        // Index the block hash and height
        self.zs_insert(&hash_by_height, height, hash);
        self.zs_insert(&height_by_hash, hash, height);

        for (transaction_index, (transaction, transaction_hash)) in block
            .transactions
            .iter()
            .zip(transaction_hashes.iter())
            .enumerate()
        {
            let transaction_location = TransactionLocation::from_usize(*height, transaction_index);

            // Commit each transaction's data
            self.zs_insert(&tx_by_loc, transaction_location, transaction);

            // Index each transaction hash and location
            self.zs_insert(&hash_by_tx_loc, transaction_location, transaction_hash);
            self.zs_insert(&tx_loc_by_hash, transaction_hash, transaction_location);
        }

        Ok(())
    }

    /// If `finalized.block` is a genesis block,
    /// prepare a database batch that finishes initializing the database,
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
        let FinalizedBlock { block, .. } = finalized;

        // Index each transaction's transparent and shielded data
        for transaction in block.transactions.iter() {
            self.prepare_nullifier_batch(db, transaction)?;

            DiskWriteBatch::update_note_commitment_trees(transaction, note_commitment_trees)?;
        }

        self.prepare_transparent_outputs_batch(db, finalized)
    }
}
