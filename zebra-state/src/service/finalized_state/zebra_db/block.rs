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
    constants::{
        MAX_BLOCK_REORG_HEIGHT, MAX_HEADER_SYNC_HEIGHT_RANGE, MAX_PRUNE_HEIGHTS_PER_COMMIT,
    },
    error::{CommitCheckpointVerifiedError, CommitHeaderRangeError},
    request::FinalizedBlock,
    service::check,
    service::finalized_state::{
        disk_db::{DiskDb, DiskWriteBatch, ReadDisk, WriteDisk},
        disk_format::{
            block::TransactionLocation,
            transparent::{AddressBalanceLocationUpdates, OutputLocation},
        },
        zebra_db::{metrics::block_precommit_metrics, ZebraDb},
        FromDisk, IntoDisk, RawBytes, PRUNING_METADATA,
    },
    HashOrHeight,
};

#[cfg(feature = "indexer")]
use crate::request::Spend;

#[cfg(test)]
mod tests;

const ZAKURA_HEADER_HASH_BY_HEIGHT: &str = "zakura_header_hash_by_height";
const ZAKURA_HEADER_HEIGHT_BY_HASH: &str = "zakura_header_height_by_hash";
const ZAKURA_HEADER_BY_HEIGHT: &str = "zakura_header_by_height";
pub const ZAKURA_HEADER_BODY_SIZE_BY_HEIGHT: &str = "zakura_header_body_size_by_height";

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct AdvertisedBodySize(u32);

impl AdvertisedBodySize {
    fn new(size: u32) -> Option<Self> {
        (size != 0).then_some(Self(size))
    }

    fn get(self) -> u32 {
        self.0
    }
}

impl IntoDisk for AdvertisedBodySize {
    type Bytes = [u8; 4];

    fn as_bytes(&self) -> Self::Bytes {
        self.0.to_be_bytes()
    }
}

impl FromDisk for AdvertisedBodySize {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let bytes = bytes
            .as_ref()
            .try_into()
            .expect("advertised body sizes are stored as u32");
        Self(u32::from_be_bytes(bytes))
    }
}

impl ZebraDb {
    // Read block methods

    /// Returns true if the database is empty.
    //
    // TODO: move this method to the tip section
    pub fn is_empty(&self) -> bool {
        self.tip().is_none()
    }

    /// Returns the tip height and hash, if there is one.
    //
    // TODO: rename to finalized_tip()
    //       move this method to the tip section
    #[allow(clippy::unwrap_in_result)]
    pub fn tip(&self) -> Option<(block::Height, block::Hash)> {
        let tx_by_loc = self.db.cf_handle("tx_by_loc").unwrap();
        let (last_tx_loc, _tx): (TransactionLocation, Transaction) =
            self.db.zs_last_key_value(&tx_by_loc)?;

        self.hash(last_tx_loc.height)
            .map(|hash| (last_tx_loc.height, hash))
    }

    /// Returns `true` if `height` is present in the finalized state.
    #[allow(clippy::unwrap_in_result)]
    pub fn contains_height(&self, height: block::Height) -> bool {
        let hash_by_height = self.db.cf_handle("hash_by_height").unwrap();

        self.db.zs_contains(&hash_by_height, &height)
    }

    /// Returns `true` if a full block body is present at `height`.
    ///
    /// Header-only commits never write transaction rows. Since valid Zcash
    /// blocks always have at least a coinbase transaction, the first
    /// transaction location is the body-availability marker.
    #[allow(clippy::unwrap_in_result)]
    pub fn contains_body_at_height(&self, height: block::Height) -> bool {
        let tx_by_loc = self.db.cf_handle("tx_by_loc").unwrap();
        let first_tx = TransactionLocation::min_for_height(height);

        self.db.zs_contains(&tx_by_loc, &first_tx)
    }

    /// Returns the advisory body-size hint for a header-only height, if known.
    ///
    /// `None` means the peer supplied the `0` unknown sentinel or no hint has been
    /// stored. This value is not consensus data.
    #[allow(clippy::unwrap_in_result)]
    pub fn advertised_body_size(&self, height: block::Height) -> Option<u32> {
        let body_size_by_height = self
            .db
            .cf_handle(ZAKURA_HEADER_BODY_SIZE_BY_HEIGHT)
            .unwrap();

        self.db
            .zs_get(&body_size_by_height, &height)
            .map(AdvertisedBodySize::get)
    }

    /// Returns the finalized hash for a given `block::Height` if it is present.
    #[allow(clippy::unwrap_in_result)]
    pub fn hash(&self, height: block::Height) -> Option<block::Hash> {
        let hash_by_height = self.db.cf_handle("hash_by_height").unwrap();
        self.db.zs_get(&hash_by_height, &height)
    }

    /// Returns the hash for `height` only if a full block body is present.
    pub fn body_hash(&self, height: block::Height) -> Option<block::Hash> {
        self.contains_body_at_height(height)
            .then(|| self.hash(height))
            .flatten()
    }

    /// Returns `true` if `hash` is present in the finalized state.
    #[allow(clippy::unwrap_in_result)]
    pub fn contains_hash(&self, hash: block::Hash) -> bool {
        self.height(hash)
            .is_some_and(|height| self.contains_body_at_height(height))
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
        if !self.contains_body_at_height(height) {
            return None;
        }

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

    /// Returns the highest header stored on disk.
    #[allow(clippy::unwrap_in_result)]
    pub fn best_header_tip(&self) -> Option<(block::Height, block::Hash)> {
        let body_tip = self.tip();
        let zakura_header_tip = self.zakura_header_tip();

        match (body_tip, zakura_header_tip) {
            (Some(body_tip), Some(header_tip)) if body_tip.0 >= header_tip.0 => Some(body_tip),
            (Some(_), Some(header_tip)) => Some(header_tip),
            (Some(body_tip), None) => Some(body_tip),
            (None, Some(header_tip)) => Some(header_tip),
            (None, None) => None,
        }
    }

    /// Returns a contiguous ascending header range from full blocks and Zakura header rows.
    pub fn headers_by_height_range(
        &self,
        start: block::Height,
        count: u32,
    ) -> Vec<(block::Height, block::Hash, Arc<block::Header>)> {
        let capped_count = count.min(MAX_HEADER_SYNC_HEIGHT_RANGE);

        let mut headers = Vec::with_capacity(
            usize::try_from(capped_count).expect("capped header count fits in usize"),
        );
        let mut height = start;

        for _ in 0..capped_count {
            let Some((hash, header)) = self.header_by_height(height) else {
                break;
            };

            headers.push((height, hash, header));

            let Ok(next_height) = height.next() else {
                break;
            };
            height = next_height;
        }

        headers
    }

    /// Returns recent header difficulty/time context in reverse height order,
    /// starting at `height`.
    pub fn recent_header_context(
        &self,
        height: block::Height,
    ) -> Vec<(
        zebra_chain::work::difficulty::CompactDifficulty,
        DateTime<Utc>,
    )> {
        let mut context = Vec::with_capacity(check::difficulty::POW_ADJUSTMENT_BLOCK_SPAN);
        let mut current_height = Some(height);

        while let Some(height) = current_height {
            let Some((_hash, header)) = self.header_by_height(height) else {
                break;
            };

            context.push((header.difficulty_threshold, header.time));
            if context.len() == check::difficulty::POW_ADJUSTMENT_BLOCK_SPAN {
                break;
            }

            current_height = height.previous().ok();
        }

        context
    }

    /// Returns header-known, body-missing heights.
    pub fn missing_block_bodies(
        &self,
        verified_block_tip: Option<block::Height>,
        best_header_tip: Option<block::Height>,
        from: block::Height,
        limit: u32,
    ) -> Vec<block::Height> {
        let Some(best_header_tip) = best_header_tip else {
            return Vec::new();
        };

        let start = verified_block_tip
            .and_then(|tip| tip.next().ok())
            .map_or(from, |first_missing| first_missing.max(from));

        if start > best_header_tip {
            return Vec::new();
        }

        let count = limit.min(best_header_tip.0.saturating_sub(start.0).saturating_add(1));

        self.headers_by_height_range(start, count)
            .into_iter()
            .map(|(height, _, _)| height)
            .filter(|height| !self.contains_body_at_height(*height))
            .take(limit as usize)
            .collect()
    }

    #[allow(clippy::unwrap_in_result)]
    fn zakura_header_tip(&self) -> Option<(block::Height, block::Hash)> {
        let hash_by_height = self.db.cf_handle(ZAKURA_HEADER_HASH_BY_HEIGHT).unwrap();
        self.db.zs_last_key_value(&hash_by_height)
    }

    #[allow(clippy::unwrap_in_result)]
    fn zakura_header_hash(&self, height: block::Height) -> Option<block::Hash> {
        let hash_by_height = self.db.cf_handle(ZAKURA_HEADER_HASH_BY_HEIGHT).unwrap();
        self.db.zs_get(&hash_by_height, &height)
    }

    #[allow(clippy::unwrap_in_result)]
    fn zakura_header_height(&self, hash: block::Hash) -> Option<block::Height> {
        let height_by_hash = self.db.cf_handle(ZAKURA_HEADER_HEIGHT_BY_HASH).unwrap();
        self.db.zs_get(&height_by_hash, &hash)
    }

    #[allow(clippy::unwrap_in_result)]
    fn zakura_header(&self, height: block::Height) -> Option<Arc<block::Header>> {
        let header_by_height = self.db.cf_handle(ZAKURA_HEADER_BY_HEIGHT).unwrap();
        self.db.zs_get(&header_by_height, &height)
    }

    fn header_hash(&self, height: block::Height) -> Option<block::Hash> {
        self.body_hash(height)
            .or_else(|| self.zakura_header_hash(height))
    }

    fn header_height(&self, hash: block::Hash) -> Option<block::Height> {
        self.contains_hash(hash)
            .then(|| self.height(hash))
            .flatten()
            .or_else(|| self.zakura_header_height(hash))
    }

    fn header_by_height(&self, height: block::Height) -> Option<(block::Hash, Arc<block::Header>)> {
        if let Some(hash) = self.body_hash(height) {
            return self
                .block_header(height.into())
                .map(|header| (hash, header));
        }

        let hash = self.zakura_header_hash(height)?;
        let header = self.zakura_header(height)?;

        Some((hash, header))
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

    /// Returns `true` if raw transaction bytes exist in the half-open height range
    /// `[from, until)`.
    pub(in super::super) fn raw_transactions_exist_in_range(
        &self,
        from: Height,
        until: Height,
    ) -> bool {
        if from >= until {
            return false;
        }

        self.raw_transactions_by_location_range(
            TransactionLocation::min_for_height(from)..TransactionLocation::min_for_height(until),
        )
        .next()
        .is_some()
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
        if !self.contains_body_at_height(height) {
            return None;
        }

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
    /// regular online pruning may advance this marker before older archive raw
    /// transactions are deleted. Checkpoint sync drains that archive backlog in
    /// bounded chunks before it skips raw transaction writes before its retention
    /// start.
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
    /// The genesis block (height 0) is never pruned. Regular online pruning
    /// starts at the current retention boundary rather than draining all
    /// historical raw transactions from height 1. Checkpoint-sync archive backlog
    /// draining is handled by [`Self::checkpoint_raw_transaction_prune_range`].
    /// Per-commit work is still bounded by [`MAX_PRUNE_HEIGHTS_PER_COMMIT`].
    ///
    /// # Correctness
    ///
    /// `retention` is always at least [`MIN_PRUNING_RETENTION`], which is strictly
    /// greater than [`MAX_BLOCK_REORG_HEIGHT`](crate::constants::MAX_BLOCK_REORG_HEIGHT).
    /// Since the returned range only ever covers heights at or below
    /// `new_tip - retention`, pruning can never delete data that a reorg or
    /// rollback could read.
    pub(in super::super) fn prune_height_range(
        new_tip: Height,
        retention: u32,
        lowest_retained: Option<Height>,
    ) -> Option<(Height, Height)> {
        let lowest_retained = lowest_retained.map(|height| height.0);
        let (from, until) = prune_height_range_inner(new_tip.0, retention, lowest_retained)?;
        Some((Height(from), Height(until)))
    }

    /// Returns a bounded raw transaction prune range for checkpoint sync after
    /// pruning is enabled on an archive database before the checkpoint retention
    /// start.
    ///
    /// Checkpoint sync can skip raw transaction writes before the start, but a
    /// single pruning marker cannot represent skipped heights above still-retained
    /// archive data. While the caller's cached archive-backlog flag is set,
    /// checkpoint commits keep raw transaction bytes and drain the backlog in
    /// bounded chunks.
    pub(in super::super) fn checkpoint_raw_transaction_prune_range(
        &self,
        skipped_until: Height,
    ) -> Option<(Height, Height)> {
        let prune_from = self.lowest_retained_height().unwrap_or(Height(1));

        if prune_from >= skipped_until {
            return None;
        }

        let prune_until = Height(
            prune_from
                .0
                .saturating_add(MAX_PRUNE_HEIGHTS_PER_COMMIT)
                .min(skipped_until.0),
        );

        Some((prune_from, prune_until))
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
        retention: RetentionPlan,
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
            retention.stores_raw_transactions(),
        )?;

        // In pruned storage mode, delete raw transaction history that has fallen
        // outside the retention window, and/or advance the pruning marker. This
        // goes in the same atomic batch as the tip advance, so pruning and the
        // tip advance are always consistent, and it reuses the single-writer
        // block commit path. In archive mode the plan is always `Store`, so this
        // is a no-op.
        retention.prepare_prune(&mut batch, self, &finalized);

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

    /// Seed or reconcile the Zakura header store from a committed full block.
    pub(crate) fn seed_zakura_header_from_committed_block(
        &self,
        height: block::Height,
        block: &Arc<block::Block>,
    ) -> Result<(), CommitHeaderRangeError> {
        let mut batch = DiskWriteBatch::new();
        batch.prepare_zakura_header_from_committed_block(&self.db, height, block)?;
        self.db
            .write(batch)
            .map_err(|error| CommitHeaderRangeError::StorageWriteError {
                error: error.to_string(),
            })
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

/// The resolved raw-transaction retention decision for committing one finalized
/// block.
///
/// Computed once per block by
/// [`FinalizedState::retention_plan`](super::super::FinalizedState::retention_plan)
/// and applied by [`ZebraDb::write_block`] without re-derivation.
///
/// Only [`RetentionPlan::Store`] occurs in archive mode; the other variants are
/// only produced in pruned storage mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum RetentionPlan {
    /// Store this block's raw transactions and prune nothing in this commit.
    ///
    /// Archive mode, or pruned mode within the retention window with nothing due.
    Store,

    /// Store this block's raw transactions, and delete the half-open raw
    /// transaction height range `[from, until)` that has aged out of the
    /// retention window (ordinary online pruning near the tip).
    Prune { from: Height, until: Height },

    /// Drain a bounded chunk `[from, until)` of pre-existing archive raw
    /// transaction backlog while checkpoint sync is before the retention start.
    ///
    /// `final_chunk` is set on the last chunk, which reaches the checkpoint skip
    /// boundary: this block's own raw transactions are skipped and the
    /// archive-backlog flag is cleared after the commit succeeds.
    DrainBacklog {
        from: Height,
        until: Height,
        final_chunk: bool,
    },

    /// Skip this checkpoint block's raw transactions (it is before the retention
    /// start with no archive backlog left to drain).
    ///
    /// Advances the pruning marker to `lowest_retained` when `write_marker` is
    /// set (i.e. the marker is not already at or ahead of it).
    Skip {
        lowest_retained: Height,
        write_marker: bool,
    },
}

impl RetentionPlan {
    /// Returns `true` when this block's raw transaction bytes should be written
    /// to `tx_by_loc`.
    pub(in super::super) fn stores_raw_transactions(self) -> bool {
        match self {
            RetentionPlan::Store | RetentionPlan::Prune { .. } => true,
            RetentionPlan::DrainBacklog { final_chunk, .. } => !final_chunk,
            RetentionPlan::Skip { .. } => false,
        }
    }

    /// Returns `true` when the archive-backlog flag should be cleared after the
    /// commit succeeds, because the backlog has been fully drained.
    pub(in super::super) fn clears_archive_backlog(self) -> bool {
        matches!(
            self,
            RetentionPlan::DrainBacklog {
                final_chunk: true,
                ..
            }
        )
    }

    /// Adds this plan's raw transaction deletes and/or pruning marker to `batch`,
    /// and logs pruning progress for operators.
    ///
    /// The block header and transaction data must already have been added to
    /// `batch` (using [`Self::stores_raw_transactions`] to decide whether raw
    /// transactions were written), so that pruning and the tip advance commit
    /// together in one atomic batch.
    pub(in super::super) fn prepare_prune(
        self,
        batch: &mut DiskWriteBatch,
        zebra_db: &ZebraDb,
        finalized: &FinalizedBlock,
    ) {
        let already_pruned = zebra_db.lowest_retained_height().is_some();
        let retention = zebra_db
            .config()
            .pruning_config()
            .map(|pruning| pruning.tx_retention);

        match self {
            RetentionPlan::Store => {}

            RetentionPlan::Prune { from, until } => {
                if should_log_prune_progress(already_pruned, finalized.height, from, until) {
                    tracing::info!(
                        prune_from = ?from,
                        prune_until = ?until,
                        tip = ?finalized.height,
                        ?retention,
                        "pruning raw transaction history outside the retention window",
                    );
                }

                batch.prepare_prune_batch(zebra_db, from, until);
            }

            RetentionPlan::DrainBacklog { from, until, .. } => {
                if should_log_prune_progress(already_pruned, finalized.height, from, until) {
                    tracing::info!(
                        prune_from = ?from,
                        prune_until = ?until,
                        tip = ?finalized.height,
                        ?retention,
                        "pruning archive raw transaction history before checkpoint skipping",
                    );
                }

                batch.prepare_prune_batch(zebra_db, from, until);
            }

            RetentionPlan::Skip {
                lowest_retained,
                write_marker,
            } => {
                if write_marker {
                    batch.prepare_pruning_marker_batch(zebra_db, lowest_retained);
                }

                debug_assert!(
                    ZebraDb::prune_height_range(
                        finalized.height,
                        retention.expect("skipping raw transactions only happens in pruned mode"),
                        zebra_db.lowest_retained_height().max(Some(lowest_retained)),
                    )
                    .is_none(),
                    "checkpoint raw transaction skipping should keep the pruning marker ahead of online pruning"
                );
            }
        }
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
        store_raw_transactions: bool,
    ) -> Result<(), CommitCheckpointVerifiedError> {
        // Commit block, transaction, and note commitment tree data.
        self.prepare_block_header_and_transaction_data_batch(
            zebra_db,
            finalized,
            store_raw_transactions,
        )?;

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
        zebra_db: &ZebraDb,
        finalized: &FinalizedBlock,
        store_raw_transactions: bool,
    ) -> Result<(), CommitCheckpointVerifiedError> {
        let db = &zebra_db.db;

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

        // Commit block header data. Full block verification is authoritative:
        // it may replace conflicting header-only provisional rows at this
        // height and truncate their header-only descendants.
        let existing_body_header: Option<Arc<block::Header>> =
            db.zs_get(&block_header_by_height, height);
        if existing_body_header.is_some_and(|existing_header| existing_header != block.header) {
            return Err(
                CommitHeaderRangeError::ConflictingFullBlockHeader { height: *height }.into(),
            );
        }

        if zebra_db
            .config()
            .enable_zakura_header_seed_from_committed_blocks
        {
            self.prepare_zakura_header_from_committed_block(db, *height, block)?;
        }

        // Index the block header, hash, and height. This also restores the
        // verified full block row after any provisional cleanup above.
        self.zs_insert(&block_header_by_height, height, &block.header);
        self.zs_insert(&hash_by_height, height, hash);
        self.zs_insert(&height_by_hash, hash, height);

        for (transaction_index, (transaction, transaction_hash)) in block
            .transactions
            .iter()
            .zip(transaction_hashes.iter())
            .enumerate()
        {
            let transaction_location = TransactionLocation::from_usize(*height, transaction_index);

            // Commit each transaction's raw bytes only when the storage policy
            // keeps historical transaction data for this height.
            if store_raw_transactions {
                self.zs_insert(&tx_by_loc, transaction_location, transaction);
            }

            // Index each transaction hash and location
            self.zs_insert(&hash_by_tx_loc, transaction_location, transaction_hash);
            self.zs_insert(&tx_loc_by_hash, transaction_hash, transaction_location);
        }

        Ok(())
    }

    /// Prepare a database batch that seeds the Zakura header store from a
    /// committed full block.
    ///
    /// Full block verification is authoritative for the stored body. If a
    /// provisional Zakura header at this height differs, replace it with the
    /// block-derived header and drop stale provisional descendants.
    #[allow(clippy::unwrap_in_result)]
    pub fn prepare_zakura_header_from_committed_block(
        &mut self,
        db: &DiskDb,
        height: block::Height,
        block: &Arc<block::Block>,
    ) -> Result<(), CommitHeaderRangeError> {
        let zakura_header_by_height = db.cf_handle(ZAKURA_HEADER_BY_HEIGHT).unwrap();
        let zakura_hash_by_height = db.cf_handle(ZAKURA_HEADER_HASH_BY_HEIGHT).unwrap();
        let zakura_height_by_hash = db.cf_handle(ZAKURA_HEADER_HEIGHT_BY_HASH).unwrap();
        let zakura_body_size_by_height = db.cf_handle(ZAKURA_HEADER_BODY_SIZE_BY_HEIGHT).unwrap();
        let tx_by_loc = db.cf_handle("tx_by_loc").unwrap();

        let hash = block.hash();
        let existing_zakura_header: Option<Arc<block::Header>> =
            db.zs_get(&zakura_header_by_height, &height);

        if existing_zakura_header.as_ref() == Some(&block.header)
            && db.zs_get::<_, _, block::Hash>(&zakura_hash_by_height, &height) == Some(hash)
        {
            return Ok(());
        }

        if existing_zakura_header.is_some_and(|existing_header| existing_header != block.header) {
            let best_header_tip: Option<(block::Height, block::Hash)> =
                db.zs_last_key_value(&zakura_hash_by_height);

            if let Some((best_header_tip, _)) = best_header_tip {
                for old_height in height.0..=best_header_tip.0 {
                    let old_height = block::Height(old_height);

                    if old_height != height
                        && db.zs_contains(
                            &tx_by_loc,
                            &TransactionLocation::min_for_height(old_height),
                        )
                    {
                        return Err(CommitHeaderRangeError::ConflictingFullBlockHeader {
                            height: old_height,
                        });
                    }

                    if let Some(old_hash) =
                        db.zs_get::<_, _, block::Hash>(&zakura_hash_by_height, &old_height)
                    {
                        self.zs_delete(&zakura_height_by_hash, old_hash);
                    }

                    self.zs_delete(&zakura_hash_by_height, old_height);
                    self.zs_delete(&zakura_header_by_height, old_height);
                    self.zs_delete(&zakura_body_size_by_height, old_height);
                }
            }
        } else if let Some(old_hash) =
            db.zs_get::<_, _, block::Hash>(&zakura_hash_by_height, &height)
        {
            if old_hash != hash {
                self.zs_delete(&zakura_height_by_hash, old_hash);
            }
        }

        self.zs_insert(&zakura_header_by_height, height, &block.header);
        self.zs_insert(&zakura_hash_by_height, height, hash);
        self.zs_insert(&zakura_height_by_hash, hash, height);

        Ok(())
    }

    /// Prepare a database batch containing a contextually validated header range.
    pub fn prepare_header_range_batch(
        &mut self,
        zebra_db: &ZebraDb,
        anchor: block::Hash,
        headers: &[Arc<block::Header>],
        body_sizes: &[u32],
    ) -> Result<block::Hash, CommitHeaderRangeError> {
        if headers.is_empty() {
            return Err(CommitHeaderRangeError::EmptyRange);
        }

        if headers.len() != body_sizes.len() {
            return Err(CommitHeaderRangeError::BodySizeCountMismatch {
                headers: headers.len(),
                body_sizes: body_sizes.len(),
            });
        }

        if headers.len() > MAX_HEADER_SYNC_HEIGHT_RANGE as usize {
            return Err(CommitHeaderRangeError::RangeTooLong {
                actual: headers.len(),
            });
        }

        let header_by_height = zebra_db.db.cf_handle(ZAKURA_HEADER_BY_HEIGHT).unwrap();
        let hash_by_height = zebra_db.db.cf_handle(ZAKURA_HEADER_HASH_BY_HEIGHT).unwrap();
        let height_by_hash = zebra_db.db.cf_handle(ZAKURA_HEADER_HEIGHT_BY_HASH).unwrap();
        let body_size_by_height = zebra_db
            .db
            .cf_handle(ZAKURA_HEADER_BODY_SIZE_BY_HEIGHT)
            .unwrap();

        let anchor_height = zebra_db
            .header_height(anchor)
            .or_else(|| (anchor == zebra_db.network().genesis_hash()).then_some(block::Height(0)))
            .ok_or(CommitHeaderRangeError::UnknownAnchor { anchor })?;

        if anchor != zebra_db.network().genesis_hash()
            && zebra_db.header_hash(anchor_height) != Some(anchor)
        {
            return Err(CommitHeaderRangeError::UnknownAnchor { anchor });
        }

        let finalized_height = zebra_db.finalized_tip_height();
        let best_header_tip = zebra_db.best_header_tip().map(|(height, _)| height);
        let checkpoints = zebra_db.network().checkpoint_list();

        let mut recent_headers = zebra_db.recent_header_context(anchor_height);
        if recent_headers.is_empty() {
            if anchor == zebra_db.network().genesis_hash() && anchor_height == block::Height(0) {
                return Err(CommitHeaderRangeError::MissingGenesisAnchor { anchor });
            }
            return Err(CommitHeaderRangeError::UnknownAnchor { anchor });
        }

        let mut first_conflicting_height = None;
        let mut validated_headers = Vec::with_capacity(headers.len());

        for (index, header) in headers.iter().enumerate() {
            let offset =
                u32::try_from(index + 1).map_err(|_| CommitHeaderRangeError::HeightOverflow)?;
            let height = (anchor_height + i64::from(offset))
                .ok_or(CommitHeaderRangeError::HeightOverflow)?;
            let hash = block::Hash::from(&**header);
            let body_size = body_sizes[index];

            if let Some(expected) = checkpoints.hash(height) {
                if expected != hash {
                    return Err(CommitHeaderRangeError::CheckpointConflict {
                        height,
                        expected,
                        actual: hash,
                    });
                }
            }

            if let Some((_existing_hash, existing_header)) = zebra_db.header_by_height(height) {
                if existing_header != *header {
                    if finalized_height.is_some_and(|finalized_height| height <= finalized_height) {
                        return Err(CommitHeaderRangeError::ImmutableConflict { height });
                    }

                    if zebra_db.contains_body_at_height(height) {
                        return Err(CommitHeaderRangeError::ConflictingFullBlockHeader { height });
                    }

                    if let Some(best_header_tip) = best_header_tip {
                        if best_header_tip.0.saturating_sub(height.0) >= MAX_BLOCK_REORG_HEIGHT {
                            return Err(CommitHeaderRangeError::ReorgTooDeep {
                                height,
                                best_header_tip,
                            });
                        }
                    }

                    first_conflicting_height.get_or_insert(height);
                }
            }

            check::header_is_valid_for_recent_chain(
                header,
                height
                    .previous()
                    .map_err(|_| CommitHeaderRangeError::HeightOverflow)?,
                &zebra_db.network(),
                recent_headers.iter().copied(),
            )?;

            recent_headers.insert(0, (header.difficulty_threshold, header.time));
            recent_headers.truncate(check::difficulty::POW_ADJUSTMENT_BLOCK_SPAN);

            validated_headers.push((height, hash, header, body_size));
        }

        if let (Some(first_conflicting_height), Some(best_header_tip)) =
            (first_conflicting_height, best_header_tip)
        {
            for height in first_conflicting_height.0..=best_header_tip.0 {
                let height = block::Height(height);

                if zebra_db.contains_body_at_height(height) {
                    return Err(CommitHeaderRangeError::ConflictingFullBlockHeader { height });
                }

                if let Some(old_hash) = zebra_db.zakura_header_hash(height) {
                    self.zs_delete(&height_by_hash, old_hash);
                }

                self.zs_delete(&hash_by_height, height);
                self.zs_delete(&header_by_height, height);
                self.zs_delete(&body_size_by_height, height);
            }
        }

        for (height, hash, header, body_size) in validated_headers {
            self.zs_insert(&header_by_height, height, header);
            self.zs_insert(&hash_by_height, height, hash);
            self.zs_insert(&height_by_hash, hash, height);
            if let Some(body_size) = AdvertisedBodySize::new(body_size) {
                self.zs_insert(&body_size_by_height, height, body_size);
            } else {
                self.zs_delete(&body_size_by_height, height);
            }
        }

        Ok(block::Hash::from(
            &**headers.last().expect("headers is non-empty"),
        ))
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
