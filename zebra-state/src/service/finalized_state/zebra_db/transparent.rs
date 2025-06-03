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
};

use rocksdb::ColumnFamily;
use zebra_chain::{
    amount::{self, Amount, NonNegative},
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
                AddressBalanceLocation, AddressBalanceLocationChange, AddressLocation,
                AddressTransaction, AddressUnspentOutput, OutputLocation,
            },
            TransactionLocation,
        },
        zebra_db::ZebraDb,
    },
    BoxError, FromDisk, IntoDisk,
};

use super::super::TypedColumnFamily;

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
    ) -> TransactionLocationBySpentOutputLocationCf {
        TransactionLocationBySpentOutputLocationCf::new(&self.db, TX_LOC_BY_SPENT_OUT_LOC)
            .expect("column family was created when database was created")
    }

    // Read transparent methods

    /// Returns the [`TransactionLocation`] for a transaction that spent the output
    /// at the provided [`OutputLocation`], if it is in the finalized state.
    pub fn tx_location_by_spent_output_location(
        &self,
        output_location: &OutputLocation,
    ) -> Option<TransactionLocation> {
        self.tx_loc_by_spent_output_loc_cf().zs_get(output_location)
    }

    /// Returns a handle to the `balance_by_transparent_addr` RocksDB column family.
    pub fn address_balance_cf(&self) -> &ColumnFamily {
        self.db.cf_handle(BALANCE_BY_TRANSPARENT_ADDR).unwrap()
    }

    /// Returns the [`AddressBalanceLocation`] for a [`transparent::Address`],
    /// if it is in the finalized state.
    #[allow(clippy::unwrap_in_result)]
    pub fn address_balance_location(
        &self,
        address: &transparent::Address,
    ) -> Option<AddressBalanceLocation> {
        let balance_by_transparent_addr = self.address_balance_cf();

        self.db.zs_get(&balance_by_transparent_addr, address)
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
            .flat_map(|&addr_out_loc| {
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
        let utxo_loc_by_transparent_addr_loc = self
            .db
            .cf_handle("utxo_loc_by_transparent_addr_loc")
            .unwrap();

        // Manually fetch the entire addresses' UTXO locations
        let mut addr_unspent_outputs = BTreeSet::new();

        // An invalid key representing the minimum possible output
        let mut unspent_output = AddressUnspentOutput::address_iterator_start(address_location);

        loop {
            // Seek to a valid entry for this address, or the first entry for the next address
            unspent_output = match self
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
        let tx_loc_by_transparent_addr_loc =
            self.db.cf_handle("tx_loc_by_transparent_addr_loc").unwrap();

        // A potentially invalid key representing the first UTXO send to the address,
        // or the query start height.
        let transaction_location_range =
            AddressTransaction::address_iterator_range(address_location, query_height_range);

        self.db
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
    ///
    /// # Errors
    ///
    /// - Propagates any errors from updating note commitment trees
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_transparent_transaction_batch(
        &mut self,
        zebra_db: &ZebraDb,
        network: &Network,
        finalized: &FinalizedBlock,
        new_outputs_by_out_loc: &BTreeMap<OutputLocation, transparent::Utxo>,
        spent_utxos_by_outpoint: &HashMap<transparent::OutPoint, transparent::Utxo>,
        spent_utxos_by_out_loc: &BTreeMap<OutputLocation, transparent::Utxo>,
        #[cfg(feature = "indexer")] out_loc_by_outpoint: &HashMap<
            transparent::OutPoint,
            OutputLocation,
        >,
        mut address_balances: HashMap<transparent::Address, AddressBalanceLocationChange>,
    ) -> Result<(), BoxError> {
        let db = &zebra_db.db;
        let FinalizedBlock { block, height, .. } = finalized;

        // Update created and spent transparent outputs
        self.prepare_new_transparent_outputs_batch(
            db,
            network,
            new_outputs_by_out_loc,
            &mut address_balances,
        )?;
        self.prepare_spent_transparent_outputs_batch(
            db,
            network,
            spent_utxos_by_out_loc,
            &mut address_balances,
        )?;

        // Index the transparent addresses that spent in each transaction
        for (tx_index, transaction) in block.transactions.iter().enumerate() {
            let spending_tx_location = TransactionLocation::from_usize(*height, tx_index);

            self.prepare_spending_transparent_tx_ids_batch(
                zebra_db,
                network,
                spending_tx_location,
                transaction,
                spent_utxos_by_outpoint,
                #[cfg(feature = "indexer")]
                out_loc_by_outpoint,
                &address_balances,
            )?;
        }

        self.prepare_transparent_balances_batch(db, address_balances)
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
    /// Also modifies the `address_balances` for these new UTXOs.
    ///
    /// # Errors
    ///
    /// - This method doesn't currently return any errors, but it might in future
    #[allow(clippy::unwrap_in_result)]
    pub fn prepare_new_transparent_outputs_batch(
        &mut self,
        db: &DiskDb,
        network: &Network,
        new_outputs_by_out_loc: &BTreeMap<OutputLocation, transparent::Utxo>,
        address_balances: &mut HashMap<transparent::Address, AddressBalanceLocationChange>,
    ) -> Result<(), BoxError> {
        let utxo_by_out_loc = db.cf_handle("utxo_by_out_loc").unwrap();
        let utxo_loc_by_transparent_addr_loc =
            db.cf_handle("utxo_loc_by_transparent_addr_loc").unwrap();
        let tx_loc_by_transparent_addr_loc =
            db.cf_handle("tx_loc_by_transparent_addr_loc").unwrap();

        // Index all new transparent outputs
        for (new_output_location, utxo) in new_outputs_by_out_loc {
            let unspent_output = &utxo.output;
            let receiving_address = unspent_output.address(network);

            // Update the address balance by adding this UTXO's value
            if let Some(receiving_address) = receiving_address {
                // TODO: fix up tests that use missing outputs,
                //       then replace entry() with get_mut().expect()

                // In memory:
                // - create the balance for the address, if needed.
                // - create or fetch the link from the address to the AddressLocation
                //   (the first location of the address in the chain).
                let address_balance_location = address_balances
                    .entry(receiving_address)
                    .or_insert_with(|| AddressBalanceLocationChange::new(*new_output_location));
                let receiving_address_location = address_balance_location.address_location();

                // Update the balance for the address in memory.
                address_balance_location
                    .receive_output(unspent_output)
                    .expect("balance overflow already checked");

                // Create a link from the AddressLocation to the new OutputLocation in the database.
                let address_unspent_output =
                    AddressUnspentOutput::new(receiving_address_location, *new_output_location);
                self.zs_insert(
                    &utxo_loc_by_transparent_addr_loc,
                    address_unspent_output,
                    (),
                );

                // Create a link from the AddressLocation to the new TransactionLocation in the database.
                // Unlike the OutputLocation link, this will never be deleted.
                let address_transaction = AddressTransaction::new(
                    receiving_address_location,
                    new_output_location.transaction_location(),
                );
                self.zs_insert(&tx_loc_by_transparent_addr_loc, address_transaction, ());
            }

            // Use the OutputLocation to store a copy of the new Output in the database.
            // (For performance reasons, we don't want to deserialize the whole transaction
            // to get an output.)
            self.zs_insert(&utxo_by_out_loc, new_output_location, unspent_output);
        }

        Ok(())
    }

    /// Prepare a database batch for the spent outputs in `spent_utxos_by_out_loc`.
    ///
    /// Adds the following changes to this batch:
    /// - delete spent UTXOs, and
    /// - delete transparent address UTXO index entries,
    ///
    /// without actually writing anything.
    ///
    /// Also modifies the `address_balances` for these new UTXOs.
    ///
    /// # Errors
    ///
    /// - This method doesn't currently return any errors, but it might in future
    #[allow(clippy::unwrap_in_result)]
    pub fn prepare_spent_transparent_outputs_batch(
        &mut self,
        db: &DiskDb,
        network: &Network,
        spent_utxos_by_out_loc: &BTreeMap<OutputLocation, transparent::Utxo>,
        address_balances: &mut HashMap<transparent::Address, AddressBalanceLocationChange>,
    ) -> Result<(), BoxError> {
        let utxo_by_out_loc = db.cf_handle("utxo_by_out_loc").unwrap();
        let utxo_loc_by_transparent_addr_loc =
            db.cf_handle("utxo_loc_by_transparent_addr_loc").unwrap();

        // Mark all transparent inputs as spent.
        //
        // Coinbase inputs represent new coins, so there are no UTXOs to mark as spent.
        for (spent_output_location, utxo) in spent_utxos_by_out_loc {
            let spent_output = &utxo.output;
            let sending_address = spent_output.address(network);

            // Fetch the balance, and the link from the address to the AddressLocation, from memory.
            if let Some(sending_address) = sending_address {
                let address_balance_location = address_balances
                    .get_mut(&sending_address)
                    .expect("spent outputs must already have an address balance");

                // Update the address balance by subtracting this UTXO's value, in memory.
                address_balance_location
                    .spend_output(spent_output)
                    .expect("balance underflow already checked");

                // Delete the link from the AddressLocation to the spent OutputLocation in the database.
                let address_spent_output = AddressUnspentOutput::new(
                    address_balance_location.address_location(),
                    *spent_output_location,
                );
                self.zs_delete(&utxo_loc_by_transparent_addr_loc, address_spent_output);
            }

            // Delete the OutputLocation, and the copy of the spent Output in the database.
            self.zs_delete(&utxo_by_out_loc, spent_output_location);
        }

        Ok(())
    }

    /// Prepare a database batch indexing the transparent addresses that spent in this transaction.
    ///
    /// Adds the following changes to this batch:
    /// - index spending transactions for each spent transparent output
    ///   (this is different from the transaction that created the output),
    ///
    /// without actually writing anything.
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
        #[cfg(feature = "indexer")] out_loc_by_outpoint: &HashMap<
            transparent::OutPoint,
            OutputLocation,
        >,
        address_balances: &HashMap<transparent::Address, AddressBalanceLocationChange>,
    ) -> Result<(), BoxError> {
        let db = &zebra_db.db;
        let tx_loc_by_transparent_addr_loc =
            db.cf_handle("tx_loc_by_transparent_addr_loc").unwrap();

        // Index the transparent addresses that spent in this transaction.
        //
        // Coinbase inputs represent new coins, so there are no UTXOs to mark as spent.
        for spent_outpoint in transaction.inputs().iter().filter_map(Input::outpoint) {
            let spent_utxo = spent_utxos_by_outpoint
                .get(&spent_outpoint)
                .expect("unexpected missing spent output");
            let sending_address = spent_utxo.output.address(network);

            // Fetch the balance, and the link from the address to the AddressLocation, from memory.
            if let Some(sending_address) = sending_address {
                let sending_address_location = address_balances
                    .get(&sending_address)
                    .expect("spent outputs must already have an address balance")
                    .address_location();

                // Create a link from the AddressLocation to the spent TransactionLocation in the database.
                // Unlike the OutputLocation link, this will never be deleted.
                //
                // The value is the location of this transaction,
                // not the transaction the spent output is from.
                let address_transaction =
                    AddressTransaction::new(sending_address_location, spending_tx_location);
                self.zs_insert(&tx_loc_by_transparent_addr_loc, address_transaction, ());
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

        Ok(())
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
        address_balances: HashMap<transparent::Address, AddressBalanceLocationChange>,
    ) -> Result<(), BoxError> {
        let balance_by_transparent_addr = db.cf_handle(BALANCE_BY_TRANSPARENT_ADDR).unwrap();

        // Update all the changed address balances in the database.
        for (address, address_balance_location_change) in address_balances.into_iter() {
            // Some of these balances are new, and some are updates
            self.zs_merge(
                &balance_by_transparent_addr,
                address,
                address_balance_location_change,
            );
        }

        Ok(())
    }
}
