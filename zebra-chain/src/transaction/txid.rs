//! Transaction ID computation. Contains code for generating the Transaction ID
//! from the transaction.

use super::{Hash, Transaction};
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
    pub(super) fn txid(self) -> Option<Hash> {
        match self.trans {
            Transaction::V1 { .. }
            | Transaction::V2 { .. }
            | Transaction::V3 { .. }
            | Transaction::V4 { .. } => self.txid_v1_to_v4(),
            Transaction::V5 { .. } => self.txid_v5(),
            #[cfg(feature = "tx_v6")]
            Transaction::V6 { .. } => self.txid_v6(),
        }
    }

    /// Compute the Transaction ID for transactions V1 to V4.
    /// In these cases it's simply the hash of the serialized transaction.
    fn txid_v1_to_v4(self) -> Option<Hash> {
        let mut hash_writer = sha256d::Writer::default();
        self.trans.zcash_serialize(&mut hash_writer).ok()?;
        Some(Hash(hash_writer.finish()))
    }

    /// Compute the Transaction ID for a V5 transaction in the given network upgrade.
    /// In this case it's the hash of a tree of hashes of specific parts of the
    /// transaction, as specified in ZIP-244 and ZIP-225.
    fn txid_v5(self) -> Option<Hash> {
        let nu = self.trans.network_upgrade()?;

        // We compute v5 txid (from ZIP-244) using librustzcash.
        Some(Hash(*self.trans.to_librustzcash(nu).ok()?.txid().as_ref()))
    }

    /// Passthrough to txid_v5 for V6 transactions.
    #[cfg(feature = "tx_v6")]
    fn txid_v6(self) -> Option<Hash> {
        self.txid_v5()
    }
}
