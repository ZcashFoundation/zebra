//! Provides high-level access to database:
//! - unspent [`transparent::Output`]s (UTXOs),
//! - spent [`transparent::Output`]s, and
//! - transparent address indexes.
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
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ops::RangeInclusive,
    sync::Arc,
};

use rocksdb::ColumnFamily;
use zebra_chain::{
    amount::{self, Amount, Constraint, NonNegative},
    block::Height,
    parameters::Network,
    transaction::{self, Transaction},
    transparent::{self, Input},
};

use crate::{
    request::FinalizedBlock,
    service::finalized_state::{
        disk_db::{DiskDb, DiskWriteBatch, ReadDisk, WriteDisk},
        disk_format::{
            transparent::{
                AddressBalanceLocation, AddressBalanceLocationChange, AddressBalanceLocationInner,
                AddressBalanceLocationUpdates, AddressLocation, AddressTransaction,
                AddressUnspentOutput, OutputLocation,
            },
            TransactionLocation,
        },
        zebra_db::ZebraDb,
    },
    FromDisk, IntoDisk,
};

use super::super::TypedColumnFamily;

/// Which parts of the transparent transaction batch to write, for the
/// consensus / RPC-only write split (see `docs/design/state-write-split.md`).
///
/// The transparent batch interleaves a consensus-critical write (the
/// `utxo_by_out_loc` unspent-output set) with the four RPC-only indexes
/// (`utxo_loc_by_transparent_addr_loc`, `tx_loc_by_transparent_addr_loc`,
/// `balance_by_transparent_addr`, and the `indexer`-feature
/// `tx_loc_by_spent_out_loc`). This selector lets the same code build either
/// half independently when the split is enabled, while remaining bit-identical
/// to the original path when it is not.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TransparentBatchKind {
    /// Write both the consensus `utxo_by_out_loc` set and the RPC-only indexes,
    /// in one batch. The single-database default — unchanged behaviour.
    Combined,

    /// Write only the consensus `utxo_by_out_loc` set; skip the RPC-only
    /// indexes and the balance read/merge. Used on the consensus write path
    /// when the split is enabled.
    ConsensusOnly,

    /// Write only the RPC-only indexes and balances; skip the consensus
    /// `utxo_by_out_loc` set. Used by the trailing RPC indexer thread when the
    /// split is enabled.
    RpcOnly,
}

impl TransparentBatchKind {
    /// Whether the consensus `utxo_by_out_loc` set should be written.
    pub fn writes_utxo_set(self) -> bool {
        matches!(self, Self::Combined | Self::ConsensusOnly)
    }

    /// Whether the RPC-only address / balance / spent-tx indexes should be
    /// written.
    pub fn writes_rpc_indexes(self) -> bool {
        matches!(self, Self::Combined | Self::RpcOnly)
    }
}

/// The name of the transaction hash by spent outpoints column family.
pub const TX_LOC_BY_SPENT_OUT_LOC: &str = "tx_loc_by_spent_out_loc";

/// The name of the [balance](AddressBalanceLocation) by transparent address column family.
pub const BALANCE_BY_TRANSPARENT_ADDR: &str = "balance_by_transparent_addr";

/// The name of the [`BALANCE_BY_TRANSPARENT_ADDR`] column family's merge operator
pub const BALANCE_BY_TRANSPARENT_ADDR_MERGE_OP: &str = "fetch_add_balance_and_received";

/// A RocksDB merge operator for the [`BALANCE_BY_TRANSPARENT_ADDR`] column family.
pub fn fetch_add_balance_and_received(
    _: &[u8],
    existing_val: Option<&[u8]>,
    operands: &rocksdb::MergeOperands,
) -> Option<Vec<u8>> {
    // # Correctness
    //
    // Merge operands are ordered, but may be combined without an existing value in partial merges, so
    // we may need to return a negative balance here.
    existing_val
        .into_iter()
        .chain(operands)
        .map(AddressBalanceLocationChange::from_bytes)
        .reduce(|a, b| (a + b).expect("address balance/received should not overflow"))
        .map(|address_balance_location| address_balance_location.as_bytes().to_vec())
}

/// The type for reading value pools from the database.
///
/// This constant should be used so the compiler can detect incorrectly typed accesses to the
/// column family.
pub type TransactionLocationBySpentOutputLocationCf<'cf> =
    TypedColumnFamily<'cf, OutputLocation, TransactionLocation>;

impl ZebraDb {
    // Column family convenience methods

    /// Returns a typed handle to the transaction location by spent output location column family.
    pub(crate) fn tx_loc_by_spent_output_loc_cf(
        &self,
    ) -> TransactionLocationBySpentOutputLocationCf<'_> {
        TransactionLocationBySpentOutputLocationCf::new(&self.db, TX_LOC_BY_SPENT_OUT_LOC)
            .expect("column family was created when database was created")
    }

    // Read transparent methods

    /// Returns the [`TransactionLocation`] for a transaction that spent the output
    /// at the provided [`OutputLocation`], if it is in the finalized state.
    ///
    /// Reads the RPC-only `tx_loc_by_spent_out_loc` column family, which lives in
    /// the separate RPC index database when the consensus / RPC write split is
    /// enabled ([`rpc_index_or_self`](ZebraDb::rpc_index_or_self)).
    pub fn tx_location_by_spent_output_location(
        &self,
        output_location: &OutputLocation,
    ) -> Option<TransactionLocation> {
        self.rpc_index_or_self()
            .tx_loc_by_spent_output_loc_cf()
            .zs_get(output_location)
    }

    /// Returns a handle to the `balance_by_transparent_addr` RocksDB column family.
    ///
    /// When the consensus / RPC write split is enabled, this is the handle in
    /// the separate RPC index database.
    pub fn address_balance_cf(&self) -> &ColumnFamily {
        self.rpc_index_or_self()
            .db
            .cf_handle(BALANCE_BY_TRANSPARENT_ADDR)
            .unwrap()
    }

    /// Returns the [`AddressBalanceLocation`] for a [`transparent::Address`],
    /// if it is in the finalized state.
    ///
    /// Reads the RPC-only `balance_by_transparent_addr` column family, which
    /// lives in the separate RPC index database when the consensus / RPC write
    /// split is enabled.
    #[allow(clippy::unwrap_in_result)]
    pub fn address_balance_location(
        &self,
        address: &transparent::Address,
    ) -> Option<AddressBalanceLocation> {
        let rpc_db = self.rpc_index_or_self();
        let balance_by_transparent_addr = rpc_db.db.cf_handle(BALANCE_BY_TRANSPARENT_ADDR).unwrap();

        rpc_db.db.zs_get(&balance_by_transparent_addr, address)
    }

    /// Returns every `(address, balance)` record in the address-balance column
    /// family, for tests (e.g. to bulk-load a snapshot's final balances).
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn all_address_balances(&self) -> Vec<(transparent::Address, AddressBalanceLocation)> {
        let balance_by_transparent_addr = self.address_balance_cf();

        self.db
            .zs_forward_range_iter::<_, transparent::Address, AddressBalanceLocation, _>(
                &balance_by_transparent_addr,
                ..,
            )
            .collect()
    }

    /// Returns the balance and received balance for a [`transparent::Address`],
    /// if it is in the finalized state.
    pub fn address_balance(
        &self,
        address: &transparent::Address,
    ) -> Option<(Amount<NonNegative>, u64)> {
        self.address_balance_location(address)
            .map(|abl| (abl.balance(), abl.received()))
    }

    /// Returns the first output that sent funds to a [`transparent::Address`],
    /// if it is in the finalized state.
    ///
    /// This location is used as an efficient index key for addresses.
    pub fn address_location(&self, address: &transparent::Address) -> Option<AddressLocation> {
        self.address_balance_location(address)
            .map(|abl| abl.address_location())
    }

    /// Returns the [`OutputLocation`] for a [`transparent::OutPoint`].
    ///
    /// This method returns the locations of spent and unspent outpoints.
    /// Returns `None` if the output was never in the finalized state.
    pub fn output_location(&self, outpoint: &transparent::OutPoint) -> Option<OutputLocation> {
        self.transaction_location(outpoint.hash)
            .map(|transaction_location| {
                OutputLocation::from_outpoint(transaction_location, outpoint)
            })
    }

    /// Returns the transparent output for a [`transparent::OutPoint`],
    /// if it is unspent in the finalized state.
    pub fn utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::OrderedUtxo> {
        let output_location = self.output_location(outpoint)?;

        self.utxo_by_location(output_location)
    }

    /// Returns the [`TransactionLocation`] of the transaction that spent the given
    /// [`transparent::OutPoint`], if it is unspent in the finalized state and its
    /// spending transaction hash has been indexed.
    pub fn spending_tx_loc(&self, outpoint: &transparent::OutPoint) -> Option<TransactionLocation> {
        let output_location = self.output_location(outpoint)?;
        self.tx_location_by_spent_output_location(&output_location)
    }

    /// Returns the transparent output for an [`OutputLocation`],
    /// if it is unspent in the finalized state.
    #[allow(clippy::unwrap_in_result)]
    pub fn utxo_by_location(
        &self,
        output_location: OutputLocation,
    ) -> Option<transparent::OrderedUtxo> {
        let utxo_by_out_loc = self.db.cf_handle("utxo_by_out_loc").unwrap();

        let output = self.db.zs_get(&utxo_by_out_loc, &output_location)?;

        let utxo = transparent::OrderedUtxo::new(
            output,
            output_location.height(),
            output_location.transaction_index().as_usize(),
        );

        Some(utxo)
    }

    /// Streams the 8-byte on-disk [`OutputLocation`] of every currently-unspent
    /// transparent output to `f`, in ascending location (height, transaction
    /// index, output index) order.
    ///
    /// The `utxo_by_out_loc` column family holds exactly the unspent set:
    /// created outputs are inserted and spent outputs deleted on every block
    /// commit, so its live keys are the unspent transparent output set at the
    /// finalized tip. Used by the IBD state-snapshot emitter to record which
    /// created outputs survive unspent to the snapshot height.
    ///
    /// Streams rather than collecting: the unspent set is millions of entries.
    pub fn for_each_unspent_output_location_bytes(&self, mut f: impl FnMut(&[u8])) {
        let utxo_by_out_loc = self.db.cf_handle("utxo_by_out_loc").unwrap();

        for (output_location, _output) in self
            .db
            .zs_forward_range_iter::<_, OutputLocation, transparent::Output, _>(
                &utxo_by_out_loc,
                ..,
            )
        {
            f(output_location.as_bytes().as_ref());
        }
    }

    /// Streams the canonical on-disk bytes of every address-balance record to
    /// `f`, in ascending address (key) order: the 21-byte
    /// [`transparent::Address`] key, then its 32-byte [`AddressBalanceLocation`]
    /// value (balance + first-output-location + received).
    ///
    /// The `balance_by_transparent_addr` column family holds exactly one record
    /// per address that has ever received funds, keyed by address, so its live
    /// entries are the address-balance set at the finalized tip. Used by the IBD
    /// P2P snapshot server to serve byte ranges of that set, and by the snapshot
    /// emitter to hash the whole set.
    ///
    /// Streams rather than collecting: the set is millions of entries. The bytes
    /// are deterministic ([`IntoDisk`] serialization), so every honest node
    /// produces a byte-identical set.
    pub fn for_each_address_balance_bytes(&self, mut f: impl FnMut(&[u8], &[u8])) {
        let balance_by_transparent_addr = self.address_balance_cf();

        for (address_bytes, value_bytes) in self
            .db
            .zs_forward_full_bytes_iter(balance_by_transparent_addr)
        {
            f(&address_bytes, &value_bytes);
        }
    }

    /// Bulk-loads a verified address-balance set into
    /// `balance_by_transparent_addr`, overwriting (inserting) each entry.
    ///
    /// Used by the snapshot-consume (assumeUTXO) sync to load the final
    /// `H_max` balances directly, instead of deriving them per block (the
    /// measured Thread 2 bottleneck). The caller is responsible for verifying
    /// the set against its pinned hash before calling this.
    ///
    /// The balances are written with plain inserts (not merge operands), which
    /// is correct because they are the authoritative final values, not
    /// per-block deltas. Written in batches of up to `batch_size` entries to
    /// bound peak memory.
    pub fn bulk_load_address_balances(
        &self,
        balances: impl IntoIterator<Item = (transparent::Address, AddressBalanceLocation)>,
        batch_size: usize,
    ) -> Result<(), rocksdb::Error> {
        let batch_size = batch_size.max(1);
        let mut batch = DiskWriteBatch::new();
        let mut pending = 0usize;

        for (address, balance) in balances {
            let balance_by_transparent_addr =
                self.db.cf_handle(BALANCE_BY_TRANSPARENT_ADDR).unwrap();
            batch.zs_insert(&balance_by_transparent_addr, address, balance);
            pending += 1;

            if pending >= batch_size {
                self.write_batch(std::mem::take(&mut batch))?;
                pending = 0;
            }
        }

        if pending > 0 {
            self.write_batch(batch)?;
        }

        Ok(())
    }

    /// Returns the unspent transparent outputs for a [`transparent::Address`],
    /// if they are in the finalized state.
    pub fn address_utxos(
        &self,
        address: &transparent::Address,
    ) -> BTreeMap<OutputLocation, transparent::Output> {
        let address_location = match self.address_location(address) {
            Some(address_location) => address_location,
            None => return BTreeMap::new(),
        };

        let output_locations = self.address_utxo_locations(address_location);

        // Ignore any outputs spent by blocks committed during this query
        output_locations
            .iter()
            .filter_map(|&addr_out_loc| {
                Some((
                    addr_out_loc.unspent_output_location(),
                    self.utxo_by_location(addr_out_loc.unspent_output_location())?
                        .utxo
                        .output,
                ))
            })
            .collect()
    }

    /// Returns the unspent transparent output locations for a [`transparent::Address`],
    /// if they are in the finalized state.
    pub fn address_utxo_locations(
        &self,
        address_location: AddressLocation,
    ) -> BTreeSet<AddressUnspentOutput> {
        let rpc_db = self.rpc_index_or_self();
        let utxo_loc_by_transparent_addr_loc = rpc_db
            .db
            .cf_handle("utxo_loc_by_transparent_addr_loc")
            .unwrap();

        // Manually fetch the entire addresses' UTXO locations
        let mut addr_unspent_outputs = BTreeSet::new();

        // An invalid key representing the minimum possible output
        let mut unspent_output = AddressUnspentOutput::address_iterator_start(address_location);

        loop {
            // Seek to a valid entry for this address, or the first entry for the next address
            unspent_output = match rpc_db
                .db
                .zs_next_key_value_from(&utxo_loc_by_transparent_addr_loc, &unspent_output)
            {
                Some((unspent_output, ())) => unspent_output,
                // We're finished with the final address in the column family
                None => break,
            };

            // We found the next address, so we're finished with this address
            if unspent_output.address_location() != address_location {
                break;
            }

            addr_unspent_outputs.insert(unspent_output);

            // A potentially invalid key representing the next possible output
            unspent_output.address_iterator_next();
        }

        addr_unspent_outputs
    }

    /// Returns the transaction hash for an [`TransactionLocation`].
    #[allow(clippy::unwrap_in_result)]
    pub fn tx_id_by_location(&self, tx_location: TransactionLocation) -> Option<transaction::Hash> {
        let hash_by_tx_loc = self.db.cf_handle("hash_by_tx_loc").unwrap();

        self.db.zs_get(&hash_by_tx_loc, &tx_location)
    }

    /// Returns the transaction IDs that sent or received funds to `address`,
    /// in the finalized chain `query_height_range`.
    ///
    /// If address has no finalized sends or receives,
    /// or the `query_height_range` is totally outside the finalized block range,
    /// returns an empty list.
    pub fn address_tx_ids(
        &self,
        address: &transparent::Address,
        query_height_range: RangeInclusive<Height>,
    ) -> BTreeMap<TransactionLocation, transaction::Hash> {
        let address_location = match self.address_location(address) {
            Some(address_location) => address_location,
            None => return BTreeMap::new(),
        };

        // Skip this address if it was first used after the end height.
        //
        // The address location is the output location of the first UTXO sent to the address,
        // and addresses can not spend funds until they receive their first UTXO.
        if address_location.height() > *query_height_range.end() {
            return BTreeMap::new();
        }

        let transaction_locations =
            self.address_transaction_locations(address_location, query_height_range);

        transaction_locations
            .iter()
            .map(|&tx_loc| {
                (
                    tx_loc.transaction_location(),
                    self.tx_id_by_location(tx_loc.transaction_location())
                        .expect("transactions whose locations are stored must exist"),
                )
            })
            .collect()
    }

    /// Returns the locations of any transactions that sent or received from a [`transparent::Address`],
    /// if they are in the finalized state.
    pub fn address_transaction_locations(
        &self,
        address_location: AddressLocation,
        query_height_range: RangeInclusive<Height>,
    ) -> BTreeSet<AddressTransaction> {
        let rpc_db = self.rpc_index_or_self();
        let tx_loc_by_transparent_addr_loc = rpc_db
            .db
            .cf_handle("tx_loc_by_transparent_addr_loc")
            .unwrap();

        // A potentially invalid key representing the first UTXO send to the address,
        // or the query start height.
        let transaction_location_range =
            AddressTransaction::address_iterator_range(address_location, query_height_range);

        rpc_db
            .db
            .zs_forward_range_iter(&tx_loc_by_transparent_addr_loc, transaction_location_range)
            .map(|(tx_loc, ())| tx_loc)
            .collect()
    }

    // Address index queries

    /// Returns the total transparent balance and received balance for `addresses` in the finalized chain.
    ///
    /// If none of the addresses have a balance, returns zeroes.
    ///
    /// # Correctness
    ///
    /// Callers should apply the non-finalized balance change for `addresses` to the returned balances.
    ///
    /// The total balances will only be correct if the non-finalized chain matches the finalized state.
    /// Specifically, the root of the partial non-finalized chain must be a child block of the finalized tip.
    pub fn partial_finalized_transparent_balance(
        &self,
        addresses: &HashSet<transparent::Address>,
    ) -> (Amount<NonNegative>, u64) {
        let balance: amount::Result<(Amount<NonNegative>, u64)> = addresses
            .iter()
            .filter_map(|address| self.address_balance(address))
            .try_fold(
                (Amount::zero(), 0),
                |(a_balance, a_received): (Amount<NonNegative>, u64), (b_balance, b_received)| {
                    let received = a_received.saturating_add(b_received);
                    Ok(((a_balance + b_balance)?, received))
                },
            );

        balance.expect(
            "unexpected amount overflow: value balances are valid, so partial sum should be valid",
        )
    }

    /// Returns the UTXOs for `addresses` in the finalized chain.
    ///
    /// If none of the addresses has finalized UTXOs, returns an empty list.
    ///
    /// # Correctness
    ///
    /// Callers should apply the non-finalized UTXO changes for `addresses` to the returned UTXOs.
    ///
    /// The UTXOs will only be correct if the non-finalized chain matches or overlaps with
    /// the finalized state.
    ///
    /// Specifically, a block in the partial chain must be a child block of the finalized tip.
    /// (But the child block does not have to be the partial chain root.)
    pub fn partial_finalized_address_utxos(
        &self,
        addresses: &HashSet<transparent::Address>,
    ) -> BTreeMap<OutputLocation, transparent::Output> {
        addresses
            .iter()
            .flat_map(|address| self.address_utxos(address))
            .collect()
    }

    /// Returns the transaction IDs that sent or received funds to `addresses`,
    /// in the finalized chain `query_height_range`.
    ///
    /// If none of the addresses has finalized sends or receives,
    /// or the `query_height_range` is totally outside the finalized block range,
    /// returns an empty list.
    ///
    /// # Correctness
    ///
    /// Callers should combine the non-finalized transactions for `addresses`
    /// with the returned transactions.
    ///
    /// The transaction IDs will only be correct if the non-finalized chain matches or overlaps with
    /// the finalized state.
    ///
    /// Specifically, a block in the partial chain must be a child block of the finalized tip.
    /// (But the child block does not have to be the partial chain root.)
    ///
    /// This condition does not apply if there is only one address.
    /// Since address transactions are only appended by blocks, and this query reads them in order,
    /// it is impossible to get inconsistent transactions for a single address.
    pub fn partial_finalized_transparent_tx_ids(
        &self,
        addresses: &HashSet<transparent::Address>,
        query_height_range: RangeInclusive<Height>,
    ) -> BTreeMap<TransactionLocation, transaction::Hash> {
        addresses
            .iter()
            .flat_map(|address| self.address_tx_ids(address, query_height_range.clone()))
            .collect()
    }
}

impl DiskWriteBatch {
    /// Prepare a database batch containing `finalized.block`'s transparent transaction indexes,
    /// and return it (without actually writing anything).
    ///
    /// If this method returns an error, it will be propagated,
    /// and the batch should not be written to the database.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_transparent_transaction_batch(
        &mut self,
        zebra_db: &ZebraDb,
        network: &Network,
        finalized: &FinalizedBlock,
        new_outputs_by_out_loc: &BTreeMap<OutputLocation, transparent::Utxo>,
        spent_utxos_by_outpoint: &HashMap<transparent::OutPoint, transparent::Utxo>,
        spent_utxos_by_out_loc: &BTreeMap<OutputLocation, transparent::Utxo>,
        out_loc_by_outpoint: &HashMap<transparent::OutPoint, OutputLocation>,
        address_balances: AddressBalanceLocationUpdates,
    ) {
        self.prepare_transparent_transaction_batch_split(
            zebra_db,
            network,
            finalized,
            new_outputs_by_out_loc,
            spent_utxos_by_outpoint,
            spent_utxos_by_out_loc,
            out_loc_by_outpoint,
            address_balances,
            TransparentBatchKind::Combined,
        );
    }

    /// Like [`prepare_transparent_transaction_batch`], but writes only the
    /// portion of the batch selected by `kind` (see [`TransparentBatchKind`]),
    /// for the consensus / RPC-only write split.
    ///
    /// With [`TransparentBatchKind::Combined`] this is bit-identical to
    /// [`prepare_transparent_transaction_batch`].
    ///
    /// [`prepare_transparent_transaction_batch`]: Self::prepare_transparent_transaction_batch
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_transparent_transaction_batch_split(
        &mut self,
        zebra_db: &ZebraDb,
        network: &Network,
        finalized: &FinalizedBlock,
        new_outputs_by_out_loc: &BTreeMap<OutputLocation, transparent::Utxo>,
        spent_utxos_by_outpoint: &HashMap<transparent::OutPoint, transparent::Utxo>,
        spent_utxos_by_out_loc: &BTreeMap<OutputLocation, transparent::Utxo>,
        out_loc_by_outpoint: &HashMap<transparent::OutPoint, OutputLocation>,
        mut address_balances: AddressBalanceLocationUpdates,
        kind: TransparentBatchKind,
    ) {
        let db = &zebra_db.db;
        let FinalizedBlock { block, height, .. } = finalized;

        // In snapshot-consume (assumeUTXO) mode with a survivor set loaded, the
        // RPC address-index and balance writes for non-survivor outputs are
        // elided (always crash-safe: those CFs are never read by spend
        // resolution or consensus). The spend-debit elision is keyed by the
        // spent output's OutputLocation, exactly like the create-credit elision,
        // so the two decisions agree by construction on the same OutputLocation.
        //
        // The genesis block has no address index, so it never reaches here.
        let snapshot_consume = zebra_db.snapshot_consume();

        // The address-balance in-memory update is only needed when RPC indexes
        // (which read `address_location()`) or balances are written.
        if kind.writes_rpc_indexes() {
            // Update the in-memory `address_balances` transaction-by-transaction, debiting inputs
            // before crediting outputs within each transaction. This ordering keeps every
            // intermediate per-address balance within the consensus range, even when the block
            // contains a same-address transparent self-spend chain whose batch credit-first
            // intermediate balance would otherwise exceed MAX_MONEY.
            Self::prepare_transparent_address_balance_updates(
                network,
                *height,
                &block.transactions,
                spent_utxos_by_outpoint,
                out_loc_by_outpoint,
                &mut address_balances,
                snapshot_consume,
            );
        }

        // Write the new and spent transparent output index entries. These passes no longer
        // touch `address_balances`; they only read each entry's `address_location()`.
        // The `utxo_by_out_loc` writes inside them are consensus-critical; the
        // address-index writes are RPC-only — `kind` selects which run.
        self.prepare_new_transparent_outputs_batch(
            db,
            network,
            new_outputs_by_out_loc,
            &address_balances,
            snapshot_consume,
            kind,
        );
        self.prepare_spent_transparent_outputs_batch(
            db,
            network,
            spent_utxos_by_out_loc,
            &address_balances,
            snapshot_consume,
            kind,
        );

        if kind.writes_rpc_indexes() {
            // Index the transparent addresses that spent in each transaction
            for (tx_index, transaction) in block.transactions.iter().enumerate() {
                let spending_tx_location = TransactionLocation::from_usize(*height, tx_index);

                self.prepare_spending_transparent_tx_ids_batch(
                    zebra_db,
                    network,
                    spending_tx_location,
                    transaction,
                    spent_utxos_by_outpoint,
                    out_loc_by_outpoint,
                    &address_balances,
                    snapshot_consume,
                );
            }

            self.prepare_transparent_balances_batch(db, address_balances);
        }
    }

    /// Update `address_balances` in memory for the transparent transfers in `transactions`,
    /// processed transaction-by-transaction in block order, debiting inputs before crediting
    /// outputs within each transaction.
    ///
    /// This mirrors `zcashd`'s `UpdateCoins` and is what allows a same-address transparent
    /// self-spend chain in one block to be applied without the intermediate per-address
    /// balance exceeding `MAX_MONEY`. For any consensus-valid block, every per-step
    /// intermediate balance stays inside the [`Amount`] constraint of the enclosing
    /// `AddressBalanceLocationUpdates` variant.
    ///
    /// This function does not touch the RocksDB batch; index writes are still handled by
    /// [`Self::prepare_new_transparent_outputs_batch`] and
    /// [`Self::prepare_spent_transparent_outputs_batch`], which read but no longer mutate
    /// `address_balances`.
    ///
    /// # Snapshot consume (assumeUTXO)
    ///
    /// When `snapshot_consume` has a survivor set loaded, the balance credit for
    /// a created non-survivor output and the matching debit for its spend are
    /// **both** skipped (keyed by the same [`OutputLocation`], so they agree by
    /// construction). This keeps the per-block balance net-zero-consistent and
    /// elides the same outputs as the address-index passes. The balance CF is
    /// never read by spend resolution or consensus, so this is always
    /// crash-safe; see [`crate::snapshot_consume`].
    #[allow(clippy::too_many_arguments)]
    fn prepare_transparent_address_balance_updates(
        network: &Network,
        height: Height,
        transactions: &[Arc<Transaction>],
        spent_utxos_by_outpoint: &HashMap<transparent::OutPoint, transparent::Utxo>,
        out_loc_by_outpoint: &HashMap<transparent::OutPoint, OutputLocation>,
        address_balances: &mut AddressBalanceLocationUpdates,
        snapshot_consume: Option<&Arc<crate::snapshot_consume::SnapshotConsumeState>>,
    ) {
        // Only elide when a survivor set is loaded. When absent, every output is
        // treated as a survivor (no elision), matching a normal sync.
        let elide = |loc: &OutputLocation| -> bool {
            snapshot_consume
                .map(|consume| consume.elide_address_index(&loc.as_bytes()))
                .unwrap_or(false)
        };

        #[allow(clippy::too_many_arguments)]
        fn update_per_tx<
            C: Constraint + Copy + std::fmt::Debug,
            T: std::ops::DerefMut<Target = AddressBalanceLocationInner<C>>
                + From<AddressBalanceLocationInner<C>>,
        >(
            addr_locs: &mut HashMap<transparent::Address, T>,
            network: &Network,
            height: Height,
            transactions: &[Arc<Transaction>],
            spent_utxos_by_outpoint: &HashMap<transparent::OutPoint, transparent::Utxo>,
            out_loc_by_outpoint: &HashMap<transparent::OutPoint, OutputLocation>,
            elide: &impl Fn(&OutputLocation) -> bool,
        ) {
            for (tx_index, transaction) in transactions.iter().enumerate() {
                // Debit transparent inputs first. Coinbase inputs have no outpoint, so
                // `filter_map(Input::outpoint)` skips them.
                for spent_outpoint in transaction.inputs().iter().filter_map(Input::outpoint) {
                    // Skip the debit iff the spent output's create-credit was
                    // elided: same OutputLocation test as the create side.
                    if let Some(spent_loc) = out_loc_by_outpoint.get(&spent_outpoint) {
                        if elide(spent_loc) {
                            continue;
                        }
                    }

                    let spent_utxo = spent_utxos_by_outpoint
                        .get(&spent_outpoint)
                        .expect("spent outpoint must already be resolved");
                    if let Some(sending_address) = spent_utxo.output.address(network) {
                        let addr_loc = addr_locs
                            .get_mut(&sending_address)
                            .expect("spent outputs must already have an address balance");

                        addr_loc
                            .spend_output(&spent_utxo.output)
                            .expect("balance underflow already checked");
                    }
                }

                // Then credit transparent outputs.
                for (output_index, output) in transaction.outputs().iter().enumerate() {
                    let new_output_location =
                        OutputLocation::from_usize(height, tx_index, output_index);

                    // Skip the credit for an elided non-survivor output.
                    if elide(&new_output_location) {
                        continue;
                    }

                    if let Some(receiving_address) = output.address(network) {
                        let addr_loc = addr_locs.entry(receiving_address).or_insert_with(|| {
                            AddressBalanceLocationInner::new(new_output_location).into()
                        });

                        addr_loc
                            .receive_output(output)
                            .expect("balance overflow already checked");
                    }
                }
            }
        }

        match address_balances {
            AddressBalanceLocationUpdates::Merge(balance_changes) => update_per_tx(
                balance_changes,
                network,
                height,
                transactions,
                spent_utxos_by_outpoint,
                out_loc_by_outpoint,
                &elide,
            ),
            AddressBalanceLocationUpdates::Insert(balances) => update_per_tx(
                balances,
                network,
                height,
                transactions,
                spent_utxos_by_outpoint,
                out_loc_by_outpoint,
                &elide,
            ),
        }
    }

    /// Prepare a database batch for the new UTXOs in `new_outputs_by_out_loc`.
    ///
    /// Adds the following changes to this batch:
    /// - insert created UTXOs,
    /// - insert transparent address UTXO index entries, and
    /// - insert transparent address transaction entries,
    ///
    /// without actually writing anything.
    ///
    /// `address_balances` must already be populated for every transparent address that
    /// receives one of these outputs (see
    /// [`Self::prepare_transparent_address_balance_updates`]); this function only reads
    /// `address_location()` from it.
    ///
    /// # Errors
    ///
    /// - This method doesn't currently return any errors, but it might in future
    ///
    /// # Snapshot consume (assumeUTXO)
    ///
    /// When `snapshot_consume` has a survivor set loaded, the RPC address-index
    /// writes (`utxo_loc_by_transparent_addr_loc`,
    /// `tx_loc_by_transparent_addr_loc`) for a created non-survivor output are
    /// skipped — always crash-safe, since those CFs are never read by spend
    /// resolution or consensus. The `utxo_by_out_loc` write itself is skipped
    /// **only** when the unsafe [`crate::snapshot_consume::SnapshotConsumeConfig::elide_utxo_bytes`]
    /// flag is set (default off): checkpoint spend resolution falls through to
    /// this CF, so eliding it is not restart-safe. See [`crate::snapshot_consume`].
    #[allow(clippy::unwrap_in_result, clippy::too_many_arguments)]
    pub fn prepare_new_transparent_outputs_batch(
        &mut self,
        db: &DiskDb,
        network: &Network,
        new_outputs_by_out_loc: &BTreeMap<OutputLocation, transparent::Utxo>,
        address_balances: &AddressBalanceLocationUpdates,
        snapshot_consume: Option<&Arc<crate::snapshot_consume::SnapshotConsumeState>>,
        kind: TransparentBatchKind,
    ) {
        let write_rpc_indexes = kind.writes_rpc_indexes();
        let write_utxo_set = kind.writes_utxo_set();

        // The consensus UTXO-set handle is only present (and only needed) when
        // writing the consensus half; the RPC address-index handles only when
        // writing the RPC half. Under the split, the database missing the other
        // half's column families must not be touched.
        let utxo_by_out_loc = write_utxo_set.then(|| db.cf_handle("utxo_by_out_loc").unwrap());
        let utxo_loc_by_transparent_addr_loc =
            write_rpc_indexes.then(|| db.cf_handle("utxo_loc_by_transparent_addr_loc").unwrap());
        let tx_loc_by_transparent_addr_loc =
            write_rpc_indexes.then(|| db.cf_handle("tx_loc_by_transparent_addr_loc").unwrap());

        // Index all new transparent outputs
        for (new_output_location, utxo) in new_outputs_by_out_loc {
            let unspent_output = &utxo.output;
            let receiving_address = unspent_output.address(network);

            // Whether to skip the RPC address-index writes for this output
            // (always crash-safe), and whether to skip the UTXO bytes (unsafe;
            // only when the flag is set). Both use the same OutputLocation, so
            // the create and spend passes agree by construction.
            let elide_addr_index = snapshot_consume
                .map(|consume| consume.elide_address_index(&new_output_location.as_bytes()))
                .unwrap_or(false);
            let elide_utxo_byte = snapshot_consume
                .map(|consume| consume.elide_utxo_byte(&new_output_location.as_bytes()))
                .unwrap_or(false);

            if let Some(receiving_address) = receiving_address {
                // The balance pass skips the credit for an elided output, so its
                // address may be absent from `address_balances`; skipping the
                // address-index writes here keeps the two passes consistent.
                if write_rpc_indexes && !elide_addr_index {
                    let receiving_address_location = match address_balances {
                        AddressBalanceLocationUpdates::Merge(balance_changes) => balance_changes
                            .get(&receiving_address)
                            .expect(
                                "address must be in address_balances after the balance update pass",
                            )
                            .address_location(),
                        AddressBalanceLocationUpdates::Insert(balances) => balances
                            .get(&receiving_address)
                            .expect(
                                "address must be in address_balances after the balance update pass",
                            )
                            .address_location(),
                    };

                    // Create a link from the AddressLocation to the new OutputLocation in the database.
                    let address_unspent_output =
                        AddressUnspentOutput::new(receiving_address_location, *new_output_location);
                    self.zs_insert(
                        utxo_loc_by_transparent_addr_loc
                            .as_ref()
                            .expect("present when writing RPC indexes"),
                        address_unspent_output,
                        (),
                    );

                    // Create a link from the AddressLocation to the new TransactionLocation in the database.
                    // Unlike the OutputLocation link, this will never be deleted.
                    let address_transaction = AddressTransaction::new(
                        receiving_address_location,
                        new_output_location.transaction_location(),
                    );
                    self.zs_insert(
                        tx_loc_by_transparent_addr_loc
                            .as_ref()
                            .expect("present when writing RPC indexes"),
                        address_transaction,
                        (),
                    );
                }
            }

            // Use the OutputLocation to store a copy of the new Output in the database.
            // (For performance reasons, we don't want to deserialize the whole transaction
            // to get an output.)
            //
            // This is the consensus `utxo_by_out_loc` set: written on the
            // consensus path (`write_utxo_set`), skipped by the RPC-only
            // indexer. Eliding the UTXO bytes is unsafe (default off): the
            // checkpoint commit path's spend resolution reads this CF as its
            // last resort.
            if write_utxo_set && !elide_utxo_byte {
                self.zs_insert(
                    utxo_by_out_loc
                        .as_ref()
                        .expect("present when writing the consensus UTXO set"),
                    new_output_location,
                    unspent_output,
                );
            }
        }
    }

    /// Prepare a database batch for the spent outputs in `spent_utxos_by_out_loc`.
    ///
    /// Adds the following changes to this batch:
    /// - delete spent UTXOs, and
    /// - delete transparent address UTXO index entries,
    ///
    /// without actually writing anything.
    ///
    /// `address_balances` must already be populated for every transparent address that
    /// spends one of these outputs (see
    /// [`Self::prepare_transparent_address_balance_updates`]); this function only reads
    /// `address_location()` from it.
    ///
    /// # Snapshot consume (assumeUTXO)
    ///
    /// When `snapshot_consume` has a survivor set loaded, the address-index
    /// delete for a spent non-survivor output is skipped — its create-side
    /// insert was elided too, so the delete would be a no-op anyway. The
    /// `utxo_by_out_loc` delete is skipped only when the unsafe
    /// `elide_utxo_bytes` flag is set (its create was then never written, so the
    /// delete is a no-op). With the flag off (default) the delete always runs,
    /// keeping the UTXO set complete for spend resolution. The spend-side and
    /// create-side decisions use the same survivor test on the same
    /// [`OutputLocation`], so they agree by construction.
    ///
    /// # Errors
    ///
    /// - This method doesn't currently return any errors, but it might in future
    #[allow(clippy::unwrap_in_result, clippy::too_many_arguments)]
    pub fn prepare_spent_transparent_outputs_batch(
        &mut self,
        db: &DiskDb,
        network: &Network,
        spent_utxos_by_out_loc: &BTreeMap<OutputLocation, transparent::Utxo>,
        address_balances: &AddressBalanceLocationUpdates,
        snapshot_consume: Option<&Arc<crate::snapshot_consume::SnapshotConsumeState>>,
        kind: TransparentBatchKind,
    ) {
        let write_rpc_indexes = kind.writes_rpc_indexes();
        let write_utxo_set = kind.writes_utxo_set();

        // Only fetch each half's column-family handle when that half is written;
        // under the split, the other half's column families are absent.
        let utxo_by_out_loc = write_utxo_set.then(|| db.cf_handle("utxo_by_out_loc").unwrap());
        let utxo_loc_by_transparent_addr_loc =
            write_rpc_indexes.then(|| db.cf_handle("utxo_loc_by_transparent_addr_loc").unwrap());

        // Mark all transparent inputs as spent.
        //
        // Coinbase inputs represent new coins, so there are no UTXOs to mark as spent.
        for (spent_output_location, utxo) in spent_utxos_by_out_loc {
            let spent_output = &utxo.output;
            let sending_address = spent_output.address(network);

            let elide_addr_index = snapshot_consume
                .map(|consume| consume.elide_address_index(&spent_output_location.as_bytes()))
                .unwrap_or(false);
            let elide_utxo_byte = snapshot_consume
                .map(|consume| consume.elide_utxo_byte(&spent_output_location.as_bytes()))
                .unwrap_or(false);

            // Fetch the link from the address to the AddressLocation, from memory.
            // For an elided spend the address may be absent from
            // `address_balances` (its create-credit was skipped), so skip the
            // address-index delete too.
            if let Some(sending_address) = sending_address {
                if write_rpc_indexes && !elide_addr_index {
                    let address_location = match address_balances {
                        AddressBalanceLocationUpdates::Merge(balance_changes) => balance_changes
                            .get(&sending_address)
                            .expect("spent outputs must already have an address balance")
                            .address_location(),
                        AddressBalanceLocationUpdates::Insert(balances) => balances
                            .get(&sending_address)
                            .expect("spent outputs must already have an address balance")
                            .address_location(),
                    };

                    // Delete the link from the AddressLocation to the spent OutputLocation in the database.
                    let address_spent_output =
                        AddressUnspentOutput::new(address_location, *spent_output_location);

                    self.zs_delete(
                        utxo_loc_by_transparent_addr_loc
                            .as_ref()
                            .expect("present when writing RPC indexes"),
                        address_spent_output,
                    );
                }
            }

            // Delete the OutputLocation, and the copy of the spent Output in the database.
            // This is the consensus `utxo_by_out_loc` set: deleted on the
            // consensus path (`write_utxo_set`), skipped by the RPC-only
            // indexer. Skipped only when the create was elided (unsafe flag):
            // the delete is then a no-op.
            if write_utxo_set && !elide_utxo_byte {
                self.zs_delete(
                    utxo_by_out_loc
                        .as_ref()
                        .expect("present when writing the consensus UTXO set"),
                    spent_output_location,
                );
            }
        }
    }

    /// Prepare a database batch indexing the transparent addresses that spent in this transaction.
    ///
    /// Adds the following changes to this batch:
    /// - index spending transactions for each spent transparent output
    ///   (this is different from the transaction that created the output),
    ///
    /// without actually writing anything.
    ///
    /// # Snapshot consume (assumeUTXO)
    ///
    /// When `snapshot_consume` has a survivor set loaded, the spending-address
    /// index write for a spent non-survivor output is skipped (its address may
    /// be absent from `address_balances` because its create-credit was elided).
    /// This is an RPC index never read by consensus, so it is crash-safe; it
    /// diverges permanently for elided spends (an accepted experiment caveat).
    ///
    /// # Errors
    ///
    /// - This method doesn't currently return any errors, but it might in future
    #[allow(clippy::unwrap_in_result, clippy::too_many_arguments)]
    pub fn prepare_spending_transparent_tx_ids_batch(
        &mut self,
        zebra_db: &ZebraDb,
        network: &Network,
        spending_tx_location: TransactionLocation,
        transaction: &Transaction,
        spent_utxos_by_outpoint: &HashMap<transparent::OutPoint, transparent::Utxo>,
        out_loc_by_outpoint: &HashMap<transparent::OutPoint, OutputLocation>,
        address_balances: &AddressBalanceLocationUpdates,
        snapshot_consume: Option<&Arc<crate::snapshot_consume::SnapshotConsumeState>>,
    ) {
        let db = &zebra_db.db;
        let tx_loc_by_transparent_addr_loc =
            db.cf_handle("tx_loc_by_transparent_addr_loc").unwrap();

        // Index the transparent addresses that spent in this transaction.
        //
        // Coinbase inputs represent new coins, so there are no UTXOs to mark as spent.
        for spent_outpoint in transaction.inputs().iter().filter_map(Input::outpoint) {
            // Skip the spending-address index for an elided non-survivor spend,
            // keyed by the spent output's location (consistent with the other
            // passes). Coinbase inputs have no output location, so they are
            // never elided.
            let elide_addr_index =
                match (snapshot_consume, out_loc_by_outpoint.get(&spent_outpoint)) {
                    (Some(consume), Some(spent_loc)) => {
                        consume.elide_address_index(&spent_loc.as_bytes())
                    }
                    _ => false,
                };

            let spent_utxo = spent_utxos_by_outpoint
                .get(&spent_outpoint)
                .expect("unexpected missing spent output");
            let sending_address = spent_utxo.output.address(network);

            // Fetch the balance, and the link from the address to the AddressLocation, from memory.
            if let Some(sending_address) = sending_address {
                if !elide_addr_index {
                    let sending_address_location = match address_balances {
                        AddressBalanceLocationUpdates::Merge(balance_changes) => balance_changes
                            .get(&sending_address)
                            .expect("spent outputs must already have an address balance")
                            .address_location(),
                        AddressBalanceLocationUpdates::Insert(balances) => balances
                            .get(&sending_address)
                            .expect("spent outputs must already have an address balance")
                            .address_location(),
                    };

                    // Create a link from the AddressLocation to the spent TransactionLocation in the database.
                    // Unlike the OutputLocation link, this will never be deleted.
                    //
                    // The value is the location of this transaction,
                    // not the transaction the spent output is from.
                    let address_transaction =
                        AddressTransaction::new(sending_address_location, spending_tx_location);
                    self.zs_insert(&tx_loc_by_transparent_addr_loc, address_transaction, ());
                }
            }

            #[cfg(feature = "indexer")]
            {
                let spent_output_location = out_loc_by_outpoint
                    .get(&spent_outpoint)
                    .expect("spent outpoints must already have output locations");

                let _ = zebra_db
                    .tx_loc_by_spent_output_loc_cf()
                    .with_batch_for_writing(self)
                    .zs_insert(spent_output_location, &spending_tx_location);
            }
        }
    }

    /// Prepare a database batch containing `finalized.block`'s:
    /// - transparent address balance changes,
    ///
    /// and return it (without actually writing anything).
    ///
    /// # Errors
    ///
    /// - This method doesn't currently return any errors, but it might in future
    #[allow(clippy::unwrap_in_result)]
    pub fn prepare_transparent_balances_batch(
        &mut self,
        db: &DiskDb,
        address_balances: AddressBalanceLocationUpdates,
    ) {
        let balance_by_transparent_addr = db.cf_handle(BALANCE_BY_TRANSPARENT_ADDR).unwrap();

        // Update all the changed address balances in the database.
        // Some of these balances are new, and some are updates
        match address_balances {
            AddressBalanceLocationUpdates::Merge(balance_changes) => {
                for (address, address_balance_location_change) in balance_changes.into_iter() {
                    self.zs_merge(
                        &balance_by_transparent_addr,
                        address,
                        address_balance_location_change,
                    );
                }
            }

            AddressBalanceLocationUpdates::Insert(balances) => {
                for (address, address_balance_location_change) in balances.into_iter() {
                    self.zs_insert(
                        &balance_by_transparent_addr,
                        address,
                        address_balance_location_change,
                    );
                }
            }
        };
    }
}
