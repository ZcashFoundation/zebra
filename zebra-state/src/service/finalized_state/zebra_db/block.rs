//! Provides high-level access to database [`Block`]s and [`Transaction`]s.
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction, and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! [`crate::constants::state_database_format_version_in_code()`] must be incremented
//! each time the database format (column, serialization, etc) changes.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ops::RangeBounds,
    sync::Arc,
};

use chrono::{DateTime, Utc};
use itertools::Itertools;

use zebra_chain::{
    amount::NonNegative,
    block::{self, Block, Height},
    orchard,
    parallel::tree::NoteCommitmentTrees,
    parameters::{Network, GENESIS_PREVIOUS_BLOCK_HASH},
    sapling,
    serialization::{CompactSizeMessage, TrustedPreallocate, ZcashSerialize as _},
    transaction::{self, Transaction},
    transparent,
    value_balance::ValueBalance,
};

use crate::{
    constants::MAX_PRUNE_HEIGHTS_PER_COMMIT,
    error::CommitCheckpointVerifiedError,
    request::FinalizedBlock,
    service::finalized_state::{
        disk_db::{DiskDb, DiskWriteBatch, ReadDisk, WriteDisk},
        disk_format::{
            block::TransactionLocation,
            transparent::{AddressBalanceLocationUpdates, OutputLocation},
        },
        zebra_db::{metrics::block_precommit_metrics, ZebraDb},
        FromDisk, RawBytes, PRUNING_METADATA,
    },
    HashOrHeight,
};

#[cfg(feature = "indexer")]
use crate::request::Spend;

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
    // TODO: rename to finalized_tip()
    //       move this method to the tip section
    #[allow(clippy::unwrap_in_result)]
    pub fn tip(&self) -> Option<(block::Height, block::Hash)> {
        let hash_by_height = self.db.cf_handle("hash_by_height").unwrap();
        self.db.zs_last_key_value(&hash_by_height)
    }

    /// Returns `true` if `height` is present in the finalized state.
    #[allow(clippy::unwrap_in_result)]
    pub fn contains_height(&self, height: block::Height) -> bool {
        let hash_by_height = self.db.cf_handle("hash_by_height").unwrap();

        self.db.zs_contains(&hash_by_height, &height)
    }

    /// Returns the finalized hash for a given `block::Height` if it is present.
    #[allow(clippy::unwrap_in_result)]
    pub fn hash(&self, height: block::Height) -> Option<block::Hash> {
        let hash_by_height = self.db.cf_handle("hash_by_height").unwrap();
        self.db.zs_get(&hash_by_height, &height)
    }

    /// Returns `true` if `hash` is present in the finalized state.
    #[allow(clippy::unwrap_in_result)]
    pub fn contains_hash(&self, hash: block::Hash) -> bool {
        let height_by_hash = self.db.cf_handle("height_by_hash").unwrap();

        self.db.zs_contains(&height_by_hash, &hash)
    }

    /// Returns the height of the given block if it exists.
    #[allow(clippy::unwrap_in_result)]
    pub fn height(&self, hash: block::Hash) -> Option<block::Height> {
        let height_by_hash = self.db.cf_handle("height_by_hash").unwrap();
        self.db.zs_get(&height_by_hash, &hash)
    }

    /// Returns the previous block hash for the given block hash in the finalized state.
    #[allow(dead_code)]
    pub fn prev_block_hash_for_hash(&self, hash: block::Hash) -> Option<block::Hash> {
        let height = self.height(hash)?;
        let prev_height = height.previous().ok()?;

        self.hash(prev_height)
    }

    /// Returns the previous block height for the given block hash in the finalized state.
    #[allow(dead_code)]
    pub fn prev_block_height_for_hash(&self, hash: block::Hash) -> Option<block::Height> {
        let height = self.height(hash)?;

        height.previous().ok()
    }

    /// Returns the [`block::Header`] with [`block::Hash`] or
    /// [`Height`], if it exists in the finalized chain.
    //
    // TODO: move this method to the start of the section
    #[allow(clippy::unwrap_in_result)]
    pub fn block_header(&self, hash_or_height: HashOrHeight) -> Option<Arc<block::Header>> {
        // Block Header
        let block_header_by_height = self.db.cf_handle("block_header_by_height").unwrap();

        let height = hash_or_height.height_or_else(|hash| self.height(hash))?;
        let header = self.db.zs_get(&block_header_by_height, &height)?;

        Some(header)
    }

    /// Returns the raw [`block::Header`] with [`block::Hash`] or [`Height`], if
    /// it exists in the finalized chain.
    #[allow(clippy::unwrap_in_result)]
    fn raw_block_header(&self, hash_or_height: HashOrHeight) -> Option<RawBytes> {
        // Block Header
        let block_header_by_height = self.db.cf_handle("block_header_by_height").unwrap();

        let height = hash_or_height.height_or_else(|hash| self.height(hash))?;
        let header: RawBytes = self.db.zs_get(&block_header_by_height, &height)?;

        Some(header)
    }

    /// Returns the [`Block`] with [`block::Hash`] or
    /// [`Height`], if it exists in the finalized chain and its raw transaction
    /// data is available.
    ///
    /// Returns `None` if the block does not exist, or if its transaction bodies
    /// are missing because they have been pruned.
    //
    // TODO: move this method to the start of the section
    #[allow(clippy::unwrap_in_result)]
    pub fn block(&self, hash_or_height: HashOrHeight) -> Option<Arc<Block>> {
        let (raw_header, raw_txs) = self.raw_block(hash_or_height)?;

        let header = Arc::<block::Header>::from_bytes(raw_header.raw_bytes());
        let transactions = raw_txs
            .iter()
            .map(|raw_tx| Arc::<Transaction>::from_bytes(raw_tx.raw_bytes()))
            .collect();

        Some(Arc::new(Block {
            header,
            transactions,
        }))
    }

    /// Returns the [`Block`] with [`block::Hash`] or [`Height`], if it exists
    /// in the finalized chain, and its serialized size, if its raw transaction
    /// data is available.
    ///
    /// Returns `None` if the block does not exist, or if its transaction bodies
    /// are missing because they have been pruned.
    #[allow(clippy::unwrap_in_result)]
    pub fn block_and_size(&self, hash_or_height: HashOrHeight) -> Option<(Arc<Block>, usize)> {
        let (raw_header, raw_txs) = self.raw_block(hash_or_height)?;

        let header = Arc::<block::Header>::from_bytes(raw_header.raw_bytes());
        let txs: Vec<_> = raw_txs
            .iter()
            .map(|raw_tx| Arc::<Transaction>::from_bytes(raw_tx.raw_bytes()))
            .collect();

        // Compute the size of the block from the size of header and size of
        // transactions. This requires summing them all and also adding the
        // size of the CompactSize-encoded transaction count.
        // See https://developer.bitcoin.org/reference/block_chain.html#serialized-blocks
        let tx_count = CompactSizeMessage::try_from(txs.len())
            .expect("must work for a previously serialized block");
        let tx_raw = tx_count
            .zcash_serialize_to_vec()
            .expect("must work for a previously serialized block");
        let size = raw_header.raw_bytes().len()
            + raw_txs
                .iter()
                .map(|raw_tx| raw_tx.raw_bytes().len())
                .sum::<usize>()
            + tx_raw.len();

        let block = Block {
            header,
            transactions: txs,
        };
        Some((Arc::new(block), size))
    }

    /// Returns the raw [`Block`] with [`block::Hash`] or
    /// [`Height`], if it exists in the finalized chain and its raw transaction
    /// data is available.
    ///
    /// Returns `None` if the block does not exist, or if its transaction bodies
    /// are missing because they have been pruned.
    #[allow(clippy::unwrap_in_result)]
    fn raw_block(&self, hash_or_height: HashOrHeight) -> Option<(RawBytes, Vec<RawBytes>)> {
        // Block
        let height = hash_or_height.height_or_else(|hash| self.height(hash))?;
        let header = self.raw_block_header(height.into())?;

        // Transactions

        let transactions: Vec<RawBytes> = self
            .raw_transactions_by_height(height)
            .map(|(_, tx)| tx)
            .collect();

        if self.raw_block_transactions_may_be_pruned(height) {
            let transaction_hashes = self.transaction_hashes_for_block(height.into())?;
            if transactions.len() != transaction_hashes.len() {
                return None;
            }
        }

        Some((header, transactions))
    }

    /// Returns `true` if `height` is in the range where raw transactions may
    /// have been pruned from `tx_by_loc`.
    fn raw_block_transactions_may_be_pruned(&self, height: Height) -> bool {
        height.0 != 0
            && self
                .lowest_retained_height()
                .is_some_and(|lowest| height < lowest)
    }

    /// Returns the Sapling [`note commitment tree`](sapling::tree::NoteCommitmentTree) specified by
    /// a hash or height, if it exists in the finalized state.
    #[allow(clippy::unwrap_in_result)]
    pub fn sapling_tree_by_hash_or_height(
        &self,
        hash_or_height: HashOrHeight,
    ) -> Option<Arc<sapling::tree::NoteCommitmentTree>> {
        let height = hash_or_height.height_or_else(|hash| self.height(hash))?;

        self.sapling_tree_by_height(&height)
    }

    /// Returns the Orchard [`note commitment tree`](orchard::tree::NoteCommitmentTree) specified by
    /// a hash or height, if it exists in the finalized state.
    #[allow(clippy::unwrap_in_result)]
    pub fn orchard_tree_by_hash_or_height(
        &self,
        hash_or_height: HashOrHeight,
    ) -> Option<Arc<orchard::tree::NoteCommitmentTree>> {
        let height = hash_or_height.height_or_else(|hash| self.height(hash))?;

        self.orchard_tree_by_height(&height)
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

    /// Returns the [`Transaction`] with [`transaction::Hash`], and its [`Height`],
    /// if a transaction with that hash exists in the finalized chain.
    #[allow(clippy::unwrap_in_result)]
    pub fn transaction(
        &self,
        hash: transaction::Hash,
    ) -> Option<(Arc<Transaction>, Height, DateTime<Utc>)> {
        let tx_by_loc = self.db.cf_handle("tx_by_loc").unwrap();

        let transaction_location = self.transaction_location(hash)?;

        let block_time = self
            .block_header(transaction_location.height.into())
            .map(|header| header.time);

        self.db
            .zs_get(&tx_by_loc, &transaction_location)
            .and_then(|tx| block_time.map(|time| (tx, transaction_location.height, time)))
    }

    /// Returns an iterator of all [`Transaction`]s for a provided block height in finalized state.
    #[allow(clippy::unwrap_in_result)]
    pub fn transactions_by_height(
        &self,
        height: Height,
    ) -> impl Iterator<Item = (TransactionLocation, Transaction)> + '_ {
        self.transactions_by_location_range(
            TransactionLocation::min_for_height(height)
                ..=TransactionLocation::max_for_height(height),
        )
    }

    /// Returns an iterator of all raw [`Transaction`]s for a provided block
    /// height in finalized state.
    #[allow(clippy::unwrap_in_result)]
    fn raw_transactions_by_height(
        &self,
        height: Height,
    ) -> impl Iterator<Item = (TransactionLocation, RawBytes)> + '_ {
        self.raw_transactions_by_location_range(
            TransactionLocation::min_for_height(height)
                ..=TransactionLocation::max_for_height(height),
        )
    }

    /// Returns an iterator of all [`Transaction`]s in the provided range
    /// of [`TransactionLocation`]s in finalized state.
    #[allow(clippy::unwrap_in_result)]
    pub fn transactions_by_location_range<R>(
        &self,
        range: R,
    ) -> impl Iterator<Item = (TransactionLocation, Transaction)> + '_
    where
        R: RangeBounds<TransactionLocation>,
    {
        let tx_by_loc = self.db.cf_handle("tx_by_loc").unwrap();
        self.db.zs_forward_range_iter(tx_by_loc, range)
    }

    /// Returns an iterator of all raw [`Transaction`]s in the provided range
    /// of [`TransactionLocation`]s in finalized state.
    #[allow(clippy::unwrap_in_result)]
    pub fn raw_transactions_by_location_range<R>(
        &self,
        range: R,
    ) -> impl Iterator<Item = (TransactionLocation, RawBytes)> + '_
    where
        R: RangeBounds<TransactionLocation>,
    {
        let tx_by_loc = self.db.cf_handle("tx_by_loc").unwrap();
        self.db.zs_forward_range_iter(tx_by_loc, range)
    }

    /// Returns the [`TransactionLocation`] for [`transaction::Hash`],
    /// if it exists in the finalized chain.
    #[allow(clippy::unwrap_in_result)]
    pub fn transaction_location(&self, hash: transaction::Hash) -> Option<TransactionLocation> {
        let tx_loc_by_hash = self.db.cf_handle("tx_loc_by_hash").unwrap();
        self.db.zs_get(&tx_loc_by_hash, &hash)
    }

    /// Returns the [`transaction::Hash`] for [`TransactionLocation`],
    /// if it exists in the finalized chain.
    #[allow(clippy::unwrap_in_result)]
    #[allow(dead_code)]
    pub fn transaction_hash(&self, location: TransactionLocation) -> Option<transaction::Hash> {
        let hash_by_tx_loc = self.db.cf_handle("hash_by_tx_loc").unwrap();
        self.db.zs_get(&hash_by_tx_loc, &location)
    }

    /// Returns the [`transaction::Hash`] of the transaction that spent or revealed the given
    /// [`transparent::OutPoint`] or nullifier, if it is spent or revealed in the finalized state.
    #[cfg(feature = "indexer")]
    pub fn spending_transaction_hash(&self, spend: &Spend) -> Option<transaction::Hash> {
        let tx_loc = match spend {
            Spend::OutPoint(outpoint) => self.spending_tx_loc(outpoint)?,
            Spend::Sprout(nullifier) => self.sprout_revealing_tx_loc(nullifier)?,
            Spend::Sapling(nullifier) => self.sapling_revealing_tx_loc(nullifier)?,
            Spend::Orchard(nullifier) => self.orchard_revealing_tx_loc(nullifier)?,
        };

        self.transaction_hash(tx_loc)
    }

    /// Returns the [`transaction::Hash`]es in the block with `hash_or_height`,
    /// if it exists in this chain.
    ///
    /// Hashes are returned in block order.
    ///
    /// Returns `None` if the block is not found.
    #[allow(clippy::unwrap_in_result)]
    pub fn transaction_hashes_for_block(
        &self,
        hash_or_height: HashOrHeight,
    ) -> Option<Arc<[transaction::Hash]>> {
        // Block
        let height = hash_or_height.height_or_else(|hash| self.height(hash))?;

        // Transaction hashes
        let hash_by_tx_loc = self.db.cf_handle("hash_by_tx_loc").unwrap();

        // Manually fetch the entire block's transaction hashes
        let mut transaction_hashes = Vec::new();

        for tx_index in 0..=Transaction::max_allocation() {
            let tx_loc = TransactionLocation::from_u64(height, tx_index);

            if let Some(tx_hash) = self.db.zs_get(&hash_by_tx_loc, &tx_loc) {
                transaction_hashes.push(tx_hash);
            } else {
                break;
            }
        }

        Some(transaction_hashes.into())
    }

    // Pruning methods

    /// Returns the next block height managed by online pruning, if the database is
    /// in pruned storage mode.
    ///
    /// Raw transactions in non-genesis blocks below this height may have been
    /// pruned. When pruning is first enabled on an existing archive database,
    /// older pre-boundary raw transactions are intentionally left intact.
    /// Returns `None` if the database has never pruned any data (it is
    /// effectively an archive database).
    pub fn lowest_retained_height(&self) -> Option<Height> {
        let pruning_metadata = self.db.cf_handle(PRUNING_METADATA)?;
        self.db.zs_get(&pruning_metadata, &())
    }

    /// Returns `true` if the database has pruned historical data, and therefore
    /// cannot be reopened in [`StorageMode::Archive`](crate::StorageMode::Archive).
    pub fn is_pruned(&self) -> bool {
        self.lowest_retained_height().is_some()
    }

    /// Returns the half-open range of block heights `[from, until)` whose raw
    /// transaction data should be pruned when committing a block at `new_tip`,
    /// given the configured `retention` window. Returns `None` if there is
    /// nothing to prune in this commit.
    ///
    /// The genesis block (height 0) is never pruned. When pruning is first enabled
    /// on an existing archive database, online pruning starts at the current
    /// retention boundary rather than draining all historical raw transactions
    /// from height 1. Per-commit work is still bounded by
    /// [`MAX_PRUNE_HEIGHTS_PER_COMMIT`].
    ///
    /// # Correctness
    ///
    /// `retention` is always at least [`MIN_PRUNING_RETENTION`], which is strictly
    /// greater than [`MAX_BLOCK_REORG_HEIGHT`](crate::constants::MAX_BLOCK_REORG_HEIGHT).
    /// Since the returned range only ever covers heights at or below
    /// `new_tip - retention`, pruning can never delete data that a reorg or
    /// rollback could read.
    fn prune_height_range(&self, new_tip: Height, retention: u32) -> Option<(Height, Height)> {
        let lowest_retained = self.lowest_retained_height().map(|height| height.0);
        let (from, until) = prune_height_range_inner(new_tip.0, retention, lowest_retained)?;
        Some((Height(from), Height(until)))
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
    /// - Propagates any errors from computing the block's chain value balance change or
    ///   from applying the change to the chain value balance
    #[allow(clippy::unwrap_in_result)]
    pub(in super::super) fn write_block(
        &mut self,
        finalized: FinalizedBlock,
        prev_note_commitment_trees: Option<NoteCommitmentTrees>,
        network: &Network,
        source: &str,
    ) -> Result<block::Hash, CommitCheckpointVerifiedError> {
        let tx_hash_indexes: HashMap<transaction::Hash, usize> = finalized
            .transaction_hashes
            .iter()
            .enumerate()
            .map(|(index, hash)| (*hash, index))
            .collect();

        // Get a list of the new UTXOs in the format we need for database updates.
        //
        // TODO: index new_outputs by TransactionLocation,
        //       simplify the spent_utxos location lookup code,
        //       and remove the extra new_outputs_by_out_loc argument
        let new_outputs_by_out_loc: BTreeMap<OutputLocation, transparent::Utxo> = finalized
            .new_outputs
            .iter()
            .map(|(outpoint, ordered_utxo)| {
                (
                    lookup_out_loc(finalized.height, outpoint, &tx_hash_indexes),
                    ordered_utxo.utxo.clone(),
                )
            })
            .collect();

        // Get a list of the spent UTXOs, before we delete any from the database
        let spent_utxos: Vec<(transparent::OutPoint, OutputLocation, transparent::Utxo)> =
            finalized
                .block
                .transactions
                .iter()
                .flat_map(|tx| tx.inputs().iter())
                .flat_map(|input| input.outpoint())
                .map(|outpoint| {
                    (
                        outpoint,
                        // Some utxos are spent in the same block, so they will be in
                        // `tx_hash_indexes` and `new_outputs`
                        self.output_location(&outpoint).unwrap_or_else(|| {
                            lookup_out_loc(finalized.height, &outpoint, &tx_hash_indexes)
                        }),
                        self.utxo(&outpoint)
                            .map(|ordered_utxo| ordered_utxo.utxo)
                            .or_else(|| {
                                finalized
                                    .new_outputs
                                    .get(&outpoint)
                                    .map(|ordered_utxo| ordered_utxo.utxo.clone())
                            })
                            .expect("already checked UTXO was in state or block"),
                    )
                })
                .collect();

        let spent_utxos_by_outpoint: HashMap<transparent::OutPoint, transparent::Utxo> =
            spent_utxos
                .iter()
                .map(|(outpoint, _output_loc, utxo)| (*outpoint, utxo.clone()))
                .collect();

        // TODO: Add `OutputLocation`s to the values in `spent_utxos_by_outpoint` to avoid creating a second hashmap with the same keys
        #[cfg(feature = "indexer")]
        let out_loc_by_outpoint: HashMap<transparent::OutPoint, OutputLocation> = spent_utxos
            .iter()
            .map(|(outpoint, out_loc, _utxo)| (*outpoint, *out_loc))
            .collect();
        let spent_utxos_by_out_loc: BTreeMap<OutputLocation, transparent::Utxo> = spent_utxos
            .into_iter()
            .map(|(_outpoint, out_loc, utxo)| (out_loc, utxo))
            .collect();

        // Get the transparent addresses with changed balances/UTXOs
        let changed_addresses: HashSet<transparent::Address> = spent_utxos_by_out_loc
            .values()
            .chain(
                finalized
                    .new_outputs
                    .values()
                    .map(|ordered_utxo| &ordered_utxo.utxo),
            )
            .filter_map(|utxo| utxo.output.address(network))
            .unique()
            .collect();

        // Get the current address balances, before the transactions in this block

        fn read_addr_locs<T, F: Fn(&transparent::Address) -> Option<T>>(
            changed_addresses: HashSet<transparent::Address>,
            f: F,
        ) -> HashMap<transparent::Address, T> {
            changed_addresses
                .into_iter()
                .filter_map(|address| Some((address, f(&address)?)))
                .collect()
        }

        // # Performance
        //
        // It's better to update entries in RocksDB with insertions over merge operations when there is no risk that
        // insertions may overwrite values that are updated concurrently in database format upgrades as inserted values
        // are quicker to read and require less background compaction.
        //
        // Reading entries that have been updated with merge ops often requires reading the latest fully-merged value,
        // reading all of the pending merge operands (potentially hundreds), and applying pending merge operands to the
        // fully-merged value such that it's much faster to read entries that have been updated with insertions than it
        // is to read entries that have been updated with merge operations.
        let address_balances: AddressBalanceLocationUpdates = if self.finished_format_upgrades() {
            AddressBalanceLocationUpdates::Insert(read_addr_locs(changed_addresses, |addr| {
                self.address_balance_location(addr)
            }))
        } else {
            AddressBalanceLocationUpdates::Merge(read_addr_locs(changed_addresses, |addr| {
                Some(self.address_balance_location(addr)?.into_new_change())
            }))
        };

        let mut batch = DiskWriteBatch::new();

        // In case of errors, propagate and do not write the batch.
        batch.prepare_block_batch(
            self,
            network,
            &finalized,
            new_outputs_by_out_loc,
            spent_utxos_by_outpoint,
            spent_utxos_by_out_loc,
            #[cfg(feature = "indexer")]
            out_loc_by_outpoint,
            address_balances,
            self.finalized_value_pool(),
            prev_note_commitment_trees,
        )?;

        // In pruned storage mode, delete raw transaction history that has fallen
        // outside the retention window. This goes in the same atomic batch as the
        // tip advance, so the prune and the tip advance are always consistent, and
        // it reuses the single-writer block commit path.
        if let Some(pruning) = self.config().pruning_config() {
            let already_pruned = self.lowest_retained_height().is_some();

            if let Some((prune_from, prune_until)) =
                self.prune_height_range(finalized.height, pruning.tx_retention)
            {
                // Log every MAX_PRUNE_HEIGHTS_PER_COMMIT block boundaries and the first prune
                // to avoid noise.
                if should_log_prune_progress(
                    already_pruned,
                    finalized.height,
                    prune_from,
                    prune_until,
                ) {
                    tracing::info!(
                        ?prune_from,
                        ?prune_until,
                        tip = ?finalized.height,
                        retention = pruning.tx_retention,
                        "pruning raw transaction history outside the retention window",
                    );
                }

                batch.prepare_prune_batch(self, prune_from, prune_until);
            }
        }

        // Track batch commit latency for observability
        let batch_start = std::time::Instant::now();
        self.db
            .write(batch)
            .expect("unexpected rocksdb error while writing block");
        metrics::histogram!("zebra.state.rocksdb.batch_commit.duration_seconds")
            .record(batch_start.elapsed().as_secs_f64());

        tracing::trace!(?source, "committed block from");

        Ok(finalized.hash)
    }

    /// Writes the given batch to the database.
    pub fn write_batch(&self, batch: DiskWriteBatch) -> Result<(), rocksdb::Error> {
        self.db.write(batch)
    }

    /// Flushes pending writes to SST files.
    pub fn flush(&self) -> Result<(), rocksdb::Error> {
        self.db.flush()
    }

    /// Compact raw transaction data in the half-open height range
    /// `[prune_from, prune_until_strictly_before)`.
    pub fn compact_raw_transaction_range(
        &self,
        prune_from: Height,
        prune_until_strictly_before: Height,
    ) {
        let tx_by_loc = self.db.cf_handle("tx_by_loc").unwrap();
        let range_start = TransactionLocation::min_for_height(prune_from);
        let range_end = TransactionLocation::min_for_height(prune_until_strictly_before);

        self.db.zs_compact_range(&tx_by_loc, range_start, range_end);
    }
}

/// Lookup the output location for an outpoint.
///
/// `tx_hash_indexes` must contain `outpoint.hash` and that transaction's index in its block.
fn lookup_out_loc(
    height: Height,
    outpoint: &transparent::OutPoint,
    tx_hash_indexes: &HashMap<transaction::Hash, usize>,
) -> OutputLocation {
    let tx_index = tx_hash_indexes
        .get(&outpoint.hash)
        .expect("already checked UTXO was in state or block");

    let tx_loc = TransactionLocation::from_usize(height, *tx_index);

    OutputLocation::from_outpoint(tx_loc, outpoint)
}

/// Computes the half-open range of block heights `[from, until)` to prune when a
/// block is committed at `new_tip`, given the `retention` window and the
/// `lowest_retained` pruning progress marker (`None` if nothing has been pruned
/// yet). Returns `None` if there is nothing to prune.
///
/// See [`ZebraDb::prune_height_range`] for the correctness invariant.
fn prune_height_range_inner(
    new_tip: u32,
    retention: u32,
    lowest_retained: Option<u32>,
) -> Option<(u32, u32)> {
    // Highest height eligible for pruning: keep `retention` blocks below the tip.
    let max_prunable = new_tip.checked_sub(retention)?;

    // Never prune the genesis block (height 0); it is special-cased throughout.
    if max_prunable == 0 {
        return None;
    }

    // Resume pruning from the existing progress marker. If pruning is first enabled
    // on an existing archive database, leave older history intact and start at the
    // current retention boundary.
    let prune_from = lowest_retained.unwrap_or(max_prunable);
    if prune_from > max_prunable {
        // Nothing new to prune yet.
        return None;
    }

    // Bound the per-commit work when draining a backlog. `prune_until` is the
    // exclusive upper bound on the pruned heights.
    let prune_until = (max_prunable + 1).min(prune_from + MAX_PRUNE_HEIGHTS_PER_COMMIT);

    Some((prune_from, prune_until))
}

/// Returns true when pruning progress should be logged for operators.
fn should_log_prune_progress(
    already_pruned: bool,
    new_tip: Height,
    prune_from: Height,
    prune_until: Height,
) -> bool {
    !already_pruned
        || prune_until.0 - prune_from.0 >= MAX_PRUNE_HEIGHTS_PER_COMMIT
        || new_tip.0 % MAX_PRUNE_HEIGHTS_PER_COMMIT == 0
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
    /// - Propagates any errors from computing the block's chain value balance change or
    ///   from applying the change to the chain value balance
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_block_batch(
        &mut self,
        zebra_db: &ZebraDb,
        network: &Network,
        finalized: &FinalizedBlock,
        new_outputs_by_out_loc: BTreeMap<OutputLocation, transparent::Utxo>,
        spent_utxos_by_outpoint: HashMap<transparent::OutPoint, transparent::Utxo>,
        spent_utxos_by_out_loc: BTreeMap<OutputLocation, transparent::Utxo>,
        #[cfg(feature = "indexer")] out_loc_by_outpoint: HashMap<
            transparent::OutPoint,
            OutputLocation,
        >,
        address_balances: AddressBalanceLocationUpdates,
        value_pool: ValueBalance<NonNegative>,
        prev_note_commitment_trees: Option<NoteCommitmentTrees>,
    ) -> Result<(), CommitCheckpointVerifiedError> {
        let db = &zebra_db.db;

        // Commit block, transaction, and note commitment tree data.
        self.prepare_block_header_and_transaction_data_batch(db, finalized);

        // The consensus rules are silent on shielded transactions in the genesis block,
        // because there aren't any in the mainnet or testnet genesis blocks.
        // So this means the genesis anchor is the same as the empty anchor,
        // which is already present from height 1 to the first shielded transaction.
        //
        // In Zebra we include the nullifiers and note commitments in the genesis block because it simplifies our code.
        self.prepare_shielded_transaction_batch(zebra_db, finalized);
        self.prepare_trees_batch(zebra_db, finalized, prev_note_commitment_trees);

        // # Consensus
        //
        // > A transaction MUST NOT spend an output of the genesis block coinbase transaction.
        // > (There is one such zero-valued output, on each of Testnet and Mainnet.)
        //
        // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
        //
        // So we ignore the genesis UTXO, transparent address index, and value pool updates
        // for the genesis block. This also ignores genesis shielded value pool updates, but there
        // aren't any of those on mainnet or testnet.
        if !finalized.height.is_min() {
            // Commit transaction indexes
            self.prepare_transparent_transaction_batch(
                zebra_db,
                network,
                finalized,
                &new_outputs_by_out_loc,
                &spent_utxos_by_outpoint,
                &spent_utxos_by_out_loc,
                #[cfg(feature = "indexer")]
                &out_loc_by_outpoint,
                address_balances,
            );
        }

        // Commit UTXOs and value pools
        self.prepare_chain_value_pools_batch(
            zebra_db,
            finalized,
            spent_utxos_by_outpoint,
            value_pool,
        )?;

        // The block has passed contextual validation, so update the metrics
        block_precommit_metrics(&finalized.block, finalized.hash, finalized.height);

        Ok(())
    }

    /// Adds deletes for pruned raw transaction data to this batch, for the
    /// half-open height range `[prune_from, prune_until_strictly_before)`, and
    /// records the new pruning progress marker.
    ///
    /// This prunes only the raw transaction bytes in `tx_by_loc`, which is the
    /// largest historical column family. Everything else is retained, including:
    ///
    /// - consensus-critical state (the UTXO set, nullifiers, anchors, note
    ///   commitment trees, history tree, and value pools), and
    /// - the transaction location indexes `tx_loc_by_hash` and `hash_by_tx_loc`.
    ///
    /// # Correctness
    ///
    /// `tx_loc_by_hash` must be retained even though it is historical lookup data:
    /// spending a UTXO resolves its outpoint to an [`OutputLocation`] via
    /// [`ZebraDb::transaction_location`], which reads `tx_loc_by_hash`. A UTXO
    /// created in an old block can be spent at any later height, so pruning that
    /// index would break validation of those spends. Only the raw transaction
    /// bytes (needed for historical RPC queries, not for validating future blocks)
    /// are safe to prune.
    ///
    /// The range to prune must satisfy the retention invariant documented on
    /// [`ZebraDb::prune_height_range`].
    pub fn prepare_prune_batch(
        &mut self,
        zebra_db: &ZebraDb,
        prune_from: Height,
        prune_until_strictly_before: Height,
    ) {
        let db = &zebra_db.db;

        let tx_by_loc = db.cf_handle("tx_by_loc").unwrap();
        let pruning_metadata = db.cf_handle(PRUNING_METADATA).unwrap();

        // Range-delete the height-keyed raw transaction column family with a
        // single tombstone over the pruned height span, which is cheap for RocksDB
        // to compact (unlike a tombstone per key).
        let range_start = TransactionLocation::min_for_height(prune_from);
        let range_end = TransactionLocation::min_for_height(prune_until_strictly_before);
        self.zs_delete_range(&tx_by_loc, range_start, range_end);

        // Record pruning progress: raw transactions below `prune_until_strictly_before`
        // (except genesis) are now pruned. Writing this entry also marks the
        // database as pruned, which is a one-way state.
        self.zs_insert(&pruning_metadata, (), prune_until_strictly_before);
    }

    /// Adds a write for the pruning progress marker to this batch.
    pub fn prepare_pruning_marker_batch(
        &mut self,
        zebra_db: &ZebraDb,
        lowest_retained_height: Height,
    ) {
        let pruning_metadata = zebra_db.db.cf_handle(PRUNING_METADATA).unwrap();

        // Writing this entry also marks the database as pruned, which is a
        // one-way state.
        self.zs_insert(&pruning_metadata, (), lowest_retained_height);
    }

    /// Prepare a database batch containing the block header and transaction data
    /// from `finalized.block`, and return it (without actually writing anything).
    #[allow(clippy::unwrap_in_result)]
    pub fn prepare_block_header_and_transaction_data_batch(
        &mut self,
        db: &DiskDb,
        finalized: &FinalizedBlock,
    ) {
        // Blocks
        let block_header_by_height = db.cf_handle("block_header_by_height").unwrap();
        let hash_by_height = db.cf_handle("hash_by_height").unwrap();
        let height_by_hash = db.cf_handle("height_by_hash").unwrap();

        // Transactions
        let tx_by_loc = db.cf_handle("tx_by_loc").unwrap();
        let hash_by_tx_loc = db.cf_handle("hash_by_tx_loc").unwrap();
        let tx_loc_by_hash = db.cf_handle("tx_loc_by_hash").unwrap();

        let FinalizedBlock {
            block,
            hash,
            height,
            transaction_hashes,
            ..
        } = finalized;

        // Commit block header data
        self.zs_insert(&block_header_by_height, height, &block.header);

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
    }

    /// Deletes the block header at `height`.
    ///
    /// This is only used by rollback tests to prove modern rollback targets do not need pre-upgrade
    /// blocks for note commitment tree replay.
    #[cfg(test)]
    pub fn delete_block_header(&mut self, zebra_db: &ZebraDb, height: Height) {
        let block_header_by_height = zebra_db.db.cf_handle("block_header_by_height").unwrap();
        self.zs_delete(&block_header_by_height, height);
    }
}
