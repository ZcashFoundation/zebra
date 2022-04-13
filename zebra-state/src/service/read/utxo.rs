//! Convenience wrappers for transparent address index UTXO queries.

use std::collections::BTreeMap;

use zebra_chain::{transaction, transparent};

use crate::{OutputLocation, TransactionLocation};

/// A convenience wrapper that efficiently stores unspent transparent outputs,
/// and the corresponding transaction IDs.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct AddressUtxos {
    /// A set of unspent transparent outputs.
    utxos: BTreeMap<OutputLocation, transparent::Output>,

    /// The transaction IDs for each [`OutputLocation`] in `utxos`.
    tx_ids: BTreeMap<TransactionLocation, transaction::Hash>,
}

impl AddressUtxos {
    /// Creates a new set of address UTXOs.
    pub fn new(
        utxos: BTreeMap<OutputLocation, transparent::Output>,
        tx_ids: BTreeMap<TransactionLocation, transaction::Hash>,
    ) -> Self {
        Self { utxos, tx_ids }
    }

    /// Returns an iterator that provides the unspent output, its location in the chain,
    /// the transaction hash, and...
    ///
    /// The UTXOs are returned in chain order, across all addresses.
    #[allow(dead_code)]
    pub fn utxos(
        &self,
    ) -> impl Iterator<Item = (&transaction::Hash, &OutputLocation, &transparent::Output)> {
        self.utxos.iter().map(|(out_loc, output)| {
            (
                self.tx_ids
                    .get(&out_loc.transaction_location())
                    .expect("address indexes are consistent"),
                out_loc,
                output,
            )
        })
    }
}
