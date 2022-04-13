//! Convenience wrappers for transparent address index UTXO queries.

use std::collections::BTreeMap;

use zebra_chain::{parameters::Network, transaction, transparent};

use crate::{OutputLocation, TransactionLocation};

/// A convenience wrapper that efficiently stores unspent transparent outputs,
/// and the corresponding transaction IDs.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct AddressUtxos {
    /// A set of unspent transparent outputs.
    utxos: BTreeMap<OutputLocation, transparent::Output>,

    /// The transaction IDs for each [`OutputLocation`] in `utxos`.
    tx_ids: BTreeMap<TransactionLocation, transaction::Hash>,

    /// The configured network for this state.
    network: Network,
}

impl AddressUtxos {
    /// Creates a new set of address UTXOs.
    pub fn new(
        network: Network,
        utxos: BTreeMap<OutputLocation, transparent::Output>,
        tx_ids: BTreeMap<TransactionLocation, transaction::Hash>,
    ) -> Self {
        Self {
            utxos,
            tx_ids,
            network,
        }
    }

    /// Returns an iterator that provides the unspent output, its transaction hash,
    /// its location in the chain, and the address it was sent to.
    ///
    /// The UTXOs are returned in chain order, across all addresses.
    #[allow(dead_code)]
    pub fn utxos(
        &self,
    ) -> impl Iterator<
        Item = (
            transparent::Address,
            &transaction::Hash,
            &OutputLocation,
            &transparent::Output,
        ),
    > {
        self.utxos.iter().map(|(out_loc, output)| {
            (
                output
                    .address(self.network)
                    .expect("address indexes only contain outputs with addresses"),
                self.tx_ids
                    .get(&out_loc.transaction_location())
                    .expect("address indexes are consistent"),
                out_loc,
                output,
            )
        })
    }
}
