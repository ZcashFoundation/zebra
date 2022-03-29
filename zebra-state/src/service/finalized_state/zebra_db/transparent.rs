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

use std::collections::{BTreeMap, HashMap};

use zebra_chain::{
    amount::{Amount, NonNegative},
    transparent,
};

use crate::{
    service::finalized_state::{
        disk_db::{DiskDb, DiskWriteBatch, ReadDisk, WriteDisk},
        disk_format::transparent::{AddressBalanceLocation, AddressLocation, OutputLocation},
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
    #[allow(dead_code)]
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
}

impl DiskWriteBatch {
    /// Prepare a database batch containing `finalized.block`'s:
    /// - transparent address balance changes,
    /// - UTXO changes, and
    /// TODO:
    /// - transparent address index changes (add in #3951, #3953),
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
        let utxo_by_outpoint = db.cf_handle("utxo_by_outpoint").unwrap();
        let balance_by_transparent_addr = db.cf_handle("balance_by_transparent_addr").unwrap();

        // Index all new transparent outputs, before deleting any we've spent
        for (output_location, utxo) in new_outputs_by_out_loc {
            let output = utxo.output;
            let receiving_address = output.address(self.network());

            // Update the address balance by adding this UTXO's value
            if let Some(receiving_address) = receiving_address {
                let address_balance = address_balances
                    .entry(receiving_address)
                    .or_insert_with(|| AddressBalanceLocation::new(output_location))
                    .balance_mut();

                let new_address_balance =
                    (*address_balance + output.value()).expect("balance overflow already checked");

                *address_balance = new_address_balance;
            }

            self.zs_insert(&utxo_by_outpoint, output_location, output);
        }

        // Mark all transparent inputs as spent.
        //
        // Coinbase inputs represent new coins, so there are no UTXOs to mark as spent.
        for (output_location, utxo) in utxos_spent_by_block {
            let spent_output = utxo.output;
            let sending_address = spent_output.address(self.network());

            // Update the address balance by subtracting this UTXO's value
            if let Some(sending_address) = sending_address {
                let address_balance = address_balances
                    .entry(sending_address)
                    .or_insert_with(|| panic!("spent outputs must already have an address balance"))
                    .balance_mut();

                let new_address_balance = (*address_balance - spent_output.value())
                    .expect("balance underflow already checked");

                *address_balance = new_address_balance;
            }

            self.zs_delete(&utxo_by_outpoint, output_location);
        }

        // Write the new address balances to the database
        for (address, address_balance) in address_balances.into_iter() {
            // Some of these balances are new, and some are updates
            self.zs_insert(&balance_by_transparent_addr, address, address_balance);
        }

        Ok(())
    }
}
