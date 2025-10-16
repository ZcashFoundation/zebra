//! Transaction ID computation. Contains code for generating the Transaction ID
//! from the transaction.
use std::io;

use super::{tx_v5_and_v6, Hash, Transaction};
use crate::serialization::{sha256d, ZcashSerialize};

/// A Transaction ID builder. It computes the transaction ID by hashing
/// different parts of the transaction, depending on the transaction version.
/// For V5 transactions, it follows [ZIP-244] and [ZIP-225].
///
/// [ZIP-244]: https://zips.z.cash/zip-0244
/// [ZIP-225]: https://zips.z.cash/zip-0225
pub(super) struct TxIdBuilder<'a> {
    trans: &'a Transaction,
}

impl<'a> TxIdBuilder<'a> {
    /// Return a new TxIdBuilder for the given transaction.
    pub fn new(trans: &'a Transaction) -> Self {
        TxIdBuilder { trans }
    }

    /// Compute the Transaction ID for the previously specified transaction.
    pub(super) fn txid(self) -> Result<Hash, io::Error> {
        match self.trans {
            Transaction::V1 { .. }
            | Transaction::V2 { .. }
            | Transaction::V3 { .. }
            | Transaction::V4 { .. } => self.txid_v1_to_v4(),
            tx_v5_and_v6! { .. } => self.txid_v5_to_v6(),
        }
    }

    /// Compute the Transaction ID for transactions V1 to V4.
    /// In these cases it's simply the hash of the serialized transaction.
    #[allow(clippy::unwrap_in_result)]
    fn txid_v1_to_v4(self) -> Result<Hash, io::Error> {
        let mut hash_writer = sha256d::Writer::default();
        self.trans
            .zcash_serialize(&mut hash_writer)
            .expect("Transactions must serialize into the hash.");
        Ok(Hash(hash_writer.finish()))
    }

    /// Compute the Transaction ID for transactions V5 to V6.
    /// In this case it's the hash of a tree of hashes of specific parts of the
    /// transaction, as specified in ZIP-244 and ZIP-225 for Txv5 and ZIP-246 for TxV6.
    fn txid_v5_to_v6(self) -> Result<Hash, io::Error> {
        // The v5 txid (from ZIP-244) is computed using librustzcash. Convert the zebra
        // transaction to a librustzcash transaction.
        let alt_tx: zcash_primitives::transaction::Transaction = self.trans.try_into()?;
        Ok(Hash(*alt_tx.txid().as_ref()))
    }
}
