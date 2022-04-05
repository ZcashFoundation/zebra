//! Provides high-level access to database:
//! - unspent [`transparent::Outputs`]s (UTXOs), and
//! - transparent address indexes.
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction, and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use zebra_chain::{
    amount::{Amount, NonNegative},
    transaction, transparent,
};

use crate::{
    service::finalized_state::{
        disk_db::{DiskDb, DiskWriteBatch, ReadDisk, WriteDisk},
        disk_format::{
            transparent::{
                AddressBalanceLocation, AddressLocation, AddressTransaction, AddressUnspentOutput,
                OutputLocation,
            },
            TransactionLocation,
        },
        zebra_db::ZebraDb,
    },
    BoxError,
};

impl ZebraDb {
    // Read transparent methods

    /// Returns the [`AddressBalanceLocation`] for a [`transparent::Address`],
    /// if it is in the finalized state.
    pub fn address_balance_location(
        &self,
        address: &transparent::Address,
    ) -> Option<AddressBalanceLocation> {
        let balance_by_transparent_addr = self.db.cf_handle("balance_by_transparent_addr").unwrap();

        self.db.zs_get(&balance_by_transparent_addr, address)
    }

    /// Returns the balance for a [`transparent::Address`],
    /// if it is in the finalized state.
    #[allow(dead_code)]
    pub fn address_balance(&self, address: &transparent::Address) -> Option<Amount<NonNegative>> {
        self.address_balance_location(address)
            .map(|abl| abl.balance())
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

    /// Returns the transparent output for an [`OutputLocation`],
    /// if it is unspent in the finalized state.
    pub fn utxo_by_location(
        &self,
        output_location: OutputLocation,
    ) -> Option<transparent::OrderedUtxo> {
        let utxo_by_out_loc = self.db.cf_handle("utxo_by_outpoint").unwrap();

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
    #[allow(dead_code)]
    pub fn address_utxos(
        &self,
        address: &transparent::Address,
    ) -> BTreeMap<OutputLocation, transparent::Utxo> {
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
                        .utxo,
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
            // A valid key representing an entry for this address or the next
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
    pub fn tx_id_by_location(&self, tx_location: TransactionLocation) -> Option<transaction::Hash> {
        let hash_by_tx_loc = self.db.cf_handle("hash_by_tx_loc").unwrap();

        self.db.zs_get(&hash_by_tx_loc, &tx_location)
    }

    /// Returns the [`transaction::Hash`]es that created or spent outputs for a [`transparent::Address`],
    /// in chain order, if they are in the finalized state.
    #[allow(dead_code)]
    pub fn address_tx_ids(
        &self,
        address: &transparent::Address,
    ) -> BTreeMap<TransactionLocation, transaction::Hash> {
        let address_location = match self.address_location(address) {
            Some(address_location) => address_location,
            None => return BTreeMap::new(),
        };

        let transaction_locations = self.address_transaction_locations(address_location);

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
    #[allow(dead_code)]
    pub fn address_transaction_locations(
        &self,
        address_location: AddressLocation,
    ) -> BTreeSet<AddressTransaction> {
        let tx_loc_by_transparent_addr_loc =
            self.db.cf_handle("tx_loc_by_transparent_addr_loc").unwrap();

        // Manually fetch the entire addresses' transaction locations
        let mut addr_transactions = BTreeSet::new();

        // An invalid key representing the minimum possible transaction
        let mut transaction_location = AddressTransaction::address_iterator_start(address_location);

        loop {
            // A valid key representing an entry for this address or the next
            transaction_location = match self
                .db
                .zs_next_key_value_from(&tx_loc_by_transparent_addr_loc, &transaction_location)
            {
                Some((unspent_output, ())) => unspent_output,
                // We're finished with the final address in the column family
                None => break,
            };

            // We found the next address, so we're finished with this address
            if transaction_location.address_location() != address_location {
                break;
            }

            addr_transactions.insert(transaction_location);

            // A potentially invalid key representing the next possible output
            transaction_location.address_iterator_next();
        }

        addr_transactions
    }
}

impl DiskWriteBatch {
    /// Prepare a database batch containing `finalized.block`'s:
    /// - transparent address balance changes,
    /// - UTXO changes, and
    /// - transparent address index changes,
    /// and return it (without actually writing anything).
    ///
    /// # Errors
    ///
    /// - This method doesn't currently return any errors, but it might in future
    pub fn prepare_transparent_outputs_batch(
        &mut self,
        db: &DiskDb,
        new_outputs_by_out_loc: BTreeMap<OutputLocation, transparent::Utxo>,
        utxos_spent_by_block: BTreeMap<OutputLocation, transparent::Utxo>,
        mut address_balances: HashMap<transparent::Address, AddressBalanceLocation>,
    ) -> Result<(), BoxError> {
        let utxo_by_out_loc = db.cf_handle("utxo_by_outpoint").unwrap();
        let utxo_loc_by_transparent_addr_loc =
            db.cf_handle("utxo_loc_by_transparent_addr_loc").unwrap();
        let tx_loc_by_transparent_addr_loc =
            db.cf_handle("tx_loc_by_transparent_addr_loc").unwrap();

        // Index all new transparent outputs, before deleting any we've spent
        for (new_output_location, utxo) in new_outputs_by_out_loc {
            let unspent_output = utxo.output;
            let receiving_address = unspent_output.address(self.network());

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
                    .or_insert_with(|| AddressBalanceLocation::new(new_output_location));
                let receiving_address_location = address_balance_location.address_location();

                // Update the balance for the address in memory.
                address_balance_location
                    .receive_output(&unspent_output)
                    .expect("balance overflow already checked");

                // Create a link from the AddressLocation to the new OutputLocation in the database.
                let address_unspent_output =
                    AddressUnspentOutput::new(receiving_address_location, new_output_location);
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

        // Mark all transparent inputs as spent.
        //
        // Coinbase inputs represent new coins, so there are no UTXOs to mark as spent.
        for (spent_output_location, utxo) in utxos_spent_by_block {
            let spent_output = utxo.output;
            let sending_address = spent_output.address(self.network());

            // Fetch the balance, and the link from the address to the AddressLocation, from memory.
            if let Some(sending_address) = sending_address {
                let address_balance_location = address_balances
                    .get_mut(&sending_address)
                    .expect("spent outputs must already have an address balance");

                // Update the address balance by subtracting this UTXO's value, in memory.
                address_balance_location
                    .spend_output(&spent_output)
                    .expect("balance underflow already checked");
                let sending_address_location = address_balance_location.address_location();

                // Delete the link from the AddressLocation to the spent OutputLocation in the database.
                let address_spent_output = AddressUnspentOutput::new(
                    address_balance_location.address_location(),
                    spent_output_location,
                );
                self.zs_delete(&utxo_loc_by_transparent_addr_loc, address_spent_output);

                // Create a link from the AddressLocation to the spent TransactionLocation in the database.
                // Unlike the OutputLocation link, this will never be deleted.
                let address_transaction = AddressTransaction::new(
                    sending_address_location,
                    spent_output_location.transaction_location(),
                );
                self.zs_insert(&tx_loc_by_transparent_addr_loc, address_transaction, ());
            }

            // Delete the OutputLocation, and the copy of the spent Output in the database.
            self.zs_delete(&utxo_by_out_loc, spent_output_location);
        }

        self.prepare_transparent_balances_batch(db, address_balances)?;

        Ok(())
    }

    /// Prepare a database batch containing `finalized.block`'s:
    /// - transparent address balance changes,
    /// and return it (without actually writing anything).
    ///
    /// # Errors
    ///
    /// - This method doesn't currently return any errors, but it might in future
    pub fn prepare_transparent_balances_batch(
        &mut self,
        db: &DiskDb,
        address_balances: HashMap<transparent::Address, AddressBalanceLocation>,
    ) -> Result<(), BoxError> {
        let balance_by_transparent_addr = db.cf_handle("balance_by_transparent_addr").unwrap();

        // Update all the changed address balances in the database.
        for (address, address_balance_location) in address_balances.into_iter() {
            // Some of these balances are new, and some are updates
            self.zs_insert(
                &balance_by_transparent_addr,
                address,
                address_balance_location,
            );
        }

        Ok(())
    }
}
