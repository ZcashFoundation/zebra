//! Contains code that interfaces with the zcash_primitives crate from
//! librustzcash.

use std::{
    convert::{TryFrom, TryInto},
    io,
};

use crate::{serialization::ZcashSerialize, transaction::Transaction};

impl TryFrom<&Transaction> for zcash_primitives::transaction::Transaction {
    type Error = io::Error;

    /// Convert a Zebra transaction into a librustzcash one.
    ///
    /// # Panics
    ///
    /// If the transaction is not V5. (Currently there is no need for this
    /// conversion for other versions.)
    fn try_from(trans: &Transaction) -> Result<Self, Self::Error> {
        let network_upgrade = match trans {
            Transaction::V5 {
                network_upgrade, ..
            } => network_upgrade,
            Transaction::V1 { .. }
            | Transaction::V2 { .. }
            | Transaction::V3 { .. }
            | Transaction::V4 { .. } => panic!("Zebra only uses librustzcash for V5 transactions"),
        };

        let serialized_tx = trans.zcash_serialize_to_vec()?;
        // The `read` method currently ignores the BranchId for V5 transactions;
        // but we use the correct BranchId anyway.
        let branch_id: u32 = network_upgrade
            .branch_id()
            .expect("Network upgrade must have a Branch ID")
            .into();
        // We've already parsed this transaction, so its network upgrade must be valid.
        let branch_id: zcash_primitives::consensus::BranchId = branch_id
            .try_into()
            .expect("zcash_primitives and Zebra have the same branch ids");
        let alt_tx =
            zcash_primitives::transaction::Transaction::read(&serialized_tx[..], branch_id)?;
        Ok(alt_tx)
    }
}
