//! Transparent address histories shared between non-finalized chain snapshots.

use std::{
    collections::{hash_map::RandomState, HashMap},
    hash::BuildHasher,
    sync::Arc,
};

use zebra_chain::transparent::Address;

use super::chain::index::TransparentTransfers;

#[cfg(test)]
mod tests;

// Bound snapshot cloning to 256 Arc increments. Writes copy only touched partitions.
const PARTITIONS: usize = 256;
type AddressMap = HashMap<Address, Arc<TransparentTransfers>>;

/// A copy-on-write transparent address index.
///
/// Clones share partitions and address histories. Updates copy only the touched
/// partitions and histories.
#[derive(Clone, Debug)]
pub(crate) struct AddressTransfers {
    // Clones must keep this seed so each address stays in the same partition.
    hash: RandomState,
    partitions: Vec<Arc<AddressMap>>,
}

impl Default for AddressTransfers {
    fn default() -> Self {
        Self {
            hash: RandomState::new(),
            partitions: (0..PARTITIONS).map(|_| Arc::new(HashMap::new())).collect(),
        }
    }
}

impl AddressTransfers {
    /// Returns the partition index for `address`.
    fn partition(&self, address: &Address) -> usize {
        // Use a randomized hash rather than attacker-controlled address bytes.
        let mask = u64::try_from(PARTITIONS - 1).expect("the partition count fits in u64");
        usize::try_from(self.hash.hash_one(address) & mask)
            .expect("a partition index fits in usize")
    }

    /// Returns the history for `address`, if it has transfers in this chain.
    pub(crate) fn get(&self, address: &Address) -> Option<&TransparentTransfers> {
        self.partitions[self.partition(address)]
            .get(address)
            .map(Arc::as_ref)
    }

    /// Returns a mutable history for `address`, inserting an empty history if needed.
    pub(crate) fn get_or_insert_mut(&mut self, address: Address) -> &mut TransparentTransfers {
        let partition = self.partition(&address);
        Arc::make_mut(
            Arc::make_mut(&mut self.partitions[partition])
                .entry(address)
                .or_default(),
        )
    }

    /// Returns a mutable history for `address`, if it has transfers in this chain.
    pub(crate) fn get_mut(&mut self, address: &Address) -> Option<&mut TransparentTransfers> {
        let partition = self.partition(address);
        Arc::make_mut(&mut self.partitions[partition])
            .get_mut(address)
            .map(Arc::make_mut)
    }

    /// Removes the history for `address`.
    pub(crate) fn remove(&mut self, address: &Address) -> Option<Arc<TransparentTransfers>> {
        let partition = self.partition(address);
        Arc::make_mut(&mut self.partitions[partition]).remove(address)
    }

    /// Returns the number of addresses with transfers in this chain.
    pub(crate) fn len(&self) -> usize {
        self.partitions
            .iter()
            .map(|partition| partition.len())
            .sum()
    }

    /// Iterates over every address history, in an unspecified order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Address, &TransparentTransfers)> {
        self.partitions.iter().flat_map(|partition| {
            partition
                .iter()
                .map(|(address, transfers)| (address, transfers.as_ref()))
        })
    }
}

impl PartialEq for AddressTransfers {
    fn eq(&self, other: &Self) -> bool {
        // Independently built indexes can partition the same entries differently.
        self.len() == other.len()
            && self
                .iter()
                .all(|(address, transfers)| other.get(address) == Some(transfers))
    }
}

impl Eq for AddressTransfers {}
