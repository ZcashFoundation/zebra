//! Provides high-level access to database:
//! - unspent [`transparent::Outputs`]s
//! - transparent address indexes
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction, and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::borrow::Borrow;

use zebra_chain::transparent;

use crate::{
    service::finalized_state::{
        disk_db::{DiskDb, DiskWriteBatch, ReadDisk, WriteDisk},
        disk_format::transparent::OutputLocation,
        zebra_db::ZebraDb,
        FinalizedBlock,
    },
    BoxError,
};

impl ZebraDb {
    // Read transparent methods

    /// Returns the `transparent::Output` pointed to by the given
    /// `transparent::OutPoint` if it is present.
    pub fn utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Utxo> {
        let utxo_by_outpoint = self.db.cf_handle("utxo_by_outpoint").unwrap();

        let output_location = OutputLocation::from_outpoint(outpoint);

        self.db.zs_get(&utxo_by_outpoint, &output_location)
    }
}

impl DiskWriteBatch {
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
            let output_location = OutputLocation::from_outpoint(outpoint);

            self.zs_insert(&utxo_by_outpoint, output_location, utxo);
        }

        // Mark all transparent inputs as spent.
        //
        // Coinbase inputs represent new coins,
        // so there are no UTXOs to mark as spent.
        for output_location in block
            .transactions
            .iter()
            .flat_map(|tx| tx.inputs())
            .flat_map(|input| input.outpoint())
            .map(|outpoint| OutputLocation::from_outpoint(&outpoint))
        {
            self.zs_delete(&utxo_by_outpoint, output_location);
        }

        Ok(())
    }
}
