//! Transparent address indexes for non-finalized chains.
//!
//! Each block's address changes are built once, when a chain adds the block, and every chain
//! containing the block shares them. Chains keep these records by height, so forks, read
//! snapshots, and finalization never copy address histories.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use zebra_chain::{
    amount::{Amount, NegativeAllowed},
    parameters::Network,
    transaction, transparent,
};

use crate::{ContextuallyVerifiedBlock, OutputLocation, TransactionLocation};

/// The transparent transfers of one non-finalized block, by address.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct BlockTransparentTransfers {
    by_address: HashMap<transparent::Address, AddressBlockTransfers>,
}

/// The transparent transfers of one address in one non-finalized block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AddressBlockTransfers {
    /// The change this block makes to the address balance.
    balance: Amount<NegativeAllowed>,

    /// The total value of the outputs this block creates for the address, saturating at
    /// `u64::MAX` like the finalized index (#10556).
    received: u64,

    /// The outputs this block creates for the address, in chain order. They are shared with the
    /// block's output map.
    created: Vec<(OutputLocation, Arc<transparent::OrderedUtxo>)>,

    /// The locations of the address outputs this block spends, in chain order.
    spent: Vec<OutputLocation>,

    /// The transactions in this block that send to or spend from the address, in chain order.
    transactions: Vec<(TransactionLocation, transaction::Hash)>,
}

impl BlockTransparentTransfers {
    /// Builds the address changes of `block`.
    ///
    /// # Panics
    ///
    /// If `block` is missing a created output, or outside tests, a spent output.
    pub(crate) fn new(block: &ContextuallyVerifiedBlock, network: &Network) -> Self {
        // Balances are summed in i128, so no partial sum can overflow.
        let mut by_address: HashMap<transparent::Address, (i128, AddressBlockTransfers)> =
            HashMap::new();

        for (index, (transaction, hash)) in block
            .block
            .transactions
            .iter()
            .zip(block.transaction_hashes.iter())
            .enumerate()
        {
            let tx_location = TransactionLocation::from_usize(block.height, index);
            let output_count = transaction
                .transparent_bundle()
                .map_or(0, |bundle| bundle.vout.len());
            for output_index in 0..output_count {
                let outpoint = transparent::OutPoint {
                    hash: *hash,
                    index: output_index.try_into().expect("valid indexes fit in u32"),
                };
                let utxo = block
                    .new_outputs
                    .get(&outpoint)
                    .expect("new_outputs contains all created UTXOs");
                let Some(address) = utxo.utxo.output.address(network) else {
                    continue;
                };

                let value = utxo.utxo.output.value();
                let (balance, transfers) =
                    address_entry(&mut by_address, address, tx_location, hash);
                *balance += i128::from(value.zatoshis());
                transfers.received = transfers.received.saturating_add(u64::from(value));
                transfers.created.push((
                    OutputLocation::from_outpoint(transaction_location(utxo), &outpoint),
                    utxo.clone(),
                ));
            }

            for spent_outpoint in transaction.spent_outpoints() {
                // TODO: fix tests to supply correct spent outputs, then turn this into an expect()
                let Some(spent_output) = block.spent_outputs.get(&spent_outpoint) else {
                    if !cfg!(test) {
                        panic!(
                            "unexpected missing spent output: all spent outputs must be indexed"
                        );
                    }
                    continue;
                };
                let Some(address) = spent_output.utxo.output.address(network) else {
                    continue;
                };

                let (balance, transfers) =
                    address_entry(&mut by_address, address, tx_location, hash);
                *balance -= i128::from(spent_output.utxo.output.value().zatoshis());
                transfers.spent.push(OutputLocation::from_outpoint(
                    transaction_location(spent_output),
                    &spent_outpoint,
                ));
            }
        }

        let by_address = by_address
            .into_iter()
            .map(|(address, (balance, mut transfers))| {
                transfers.balance = Amount::try_from(balance)
                    .expect("one block can't move more than the money supply for an address");
                transfers.spent.sort_unstable();
                (address, transfers)
            })
            .collect();

        Self { by_address }
    }

    /// Returns the transfers of each of `addresses` that this block changes.
    pub(crate) fn for_addresses<'a>(
        &'a self,
        addresses: &'a HashSet<transparent::Address>,
    ) -> impl Iterator<Item = &'a AddressBlockTransfers> + 'a {
        // Look up whichever side is smaller.
        let (by_lookup, by_scan) = if addresses.len() <= self.by_address.len() {
            (Some(addresses), None)
        } else {
            (None, Some(addresses))
        };

        let looked_up = by_lookup
            .into_iter()
            .flatten()
            .filter_map(|address| self.by_address.get(address));
        let scanned = by_scan.into_iter().flat_map(move |addresses| {
            self.by_address
                .iter()
                .filter(move |(address, _)| addresses.contains(*address))
                .map(|(_, transfers)| transfers)
        });

        looked_up.chain(scanned)
    }

    /// Returns the number of addresses this block changes.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.by_address.len()
    }

    /// Returns the transfers of `address` in this block, if it changes the address.
    #[cfg(test)]
    pub(crate) fn get(&self, address: &transparent::Address) -> Option<&AddressBlockTransfers> {
        self.by_address.get(address)
    }
}

/// Returns the balance accumulator and transfers of `address` in a block being indexed, recording
/// that the transaction at `tx_location` changes the address.
fn address_entry<'a>(
    by_address: &'a mut HashMap<transparent::Address, (i128, AddressBlockTransfers)>,
    address: transparent::Address,
    tx_location: TransactionLocation,
    hash: &transaction::Hash,
) -> (&'a mut i128, &'a mut AddressBlockTransfers) {
    let (balance, transfers) = by_address.entry(address).or_insert_with(|| {
        (
            0,
            AddressBlockTransfers {
                balance: Amount::zero(),
                received: 0,
                created: Vec::new(),
                spent: Vec::new(),
                transactions: Vec::new(),
            },
        )
    });
    if transfers.transactions.last().map(|(location, _)| *location) != Some(tx_location) {
        transfers.transactions.push((tx_location, *hash));
    }
    (balance, transfers)
}

impl AddressBlockTransfers {
    /// Returns the change this block makes to the address balance.
    pub(crate) fn balance(&self) -> Amount<NegativeAllowed> {
        self.balance
    }

    /// Returns the total value of the outputs this block creates for the address.
    pub(crate) fn received(&self) -> u64 {
        self.received
    }

    /// Returns the outputs this block creates for the address, in chain order.
    pub(crate) fn created(&self) -> &[(OutputLocation, Arc<transparent::OrderedUtxo>)] {
        &self.created
    }

    /// Returns the locations of the address outputs this block spends, in chain order.
    pub(crate) fn spent(&self) -> &[OutputLocation] {
        &self.spent
    }

    /// Returns the transactions in this block that send to or spend from the address, in chain
    /// order.
    pub(crate) fn transactions(&self) -> &[(TransactionLocation, transaction::Hash)] {
        &self.transactions
    }
}

/// Returns the transaction location for an [`transparent::OrderedUtxo`].
pub fn transaction_location(ordered_utxo: &transparent::OrderedUtxo) -> TransactionLocation {
    TransactionLocation::from_usize(ordered_utxo.utxo.height, ordered_utxo.tx_index_in_block)
}

#[cfg(test)]
pub(in crate::service::non_finalized_state) mod tests;
