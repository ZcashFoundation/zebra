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
/// Updates copy touched partitions and histories, but share created-output entries at other heights.
#[derive(Clone, Debug)]
pub(crate) struct AddressTransfers {
    // Clones keep this seed so each address stays in the same partition.
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
    fn partition(&self, address: &Address) -> usize {
        // Randomize partition selection rather than trusting attacker-controlled address bytes.
        let mask = u64::try_from(PARTITIONS - 1).expect("the partition count fits in u64");
        usize::try_from(self.hash.hash_one(address) & mask)
            .expect("a partition index fits in usize")
    }

    pub(crate) fn get(&self, address: &Address) -> Option<&TransparentTransfers> {
        self.partitions[self.partition(address)]
            .get(address)
            .map(Arc::as_ref)
    }

    pub(crate) fn get_or_insert_mut(&mut self, address: Address) -> &mut TransparentTransfers {
        let partition = self.partition(&address);
        Arc::make_mut(
            Arc::make_mut(&mut self.partitions[partition])
                .entry(address)
                .or_default(),
        )
    }

    pub(crate) fn get_mut(&mut self, address: &Address) -> Option<&mut TransparentTransfers> {
        let partition = self.partition(address);
        Arc::make_mut(&mut self.partitions[partition])
            .get_mut(address)
            .map(Arc::make_mut)
    }

    pub(crate) fn remove(&mut self, address: &Address) -> Option<Arc<TransparentTransfers>> {
        let partition = self.partition(address);
        Arc::make_mut(&mut self.partitions[partition]).remove(address)
    }

    pub(crate) fn len(&self) -> usize {
        self.partitions
            .iter()
            .map(|partition| partition.len())
            .sum()
    }

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
        // Independently built indexes can partition equal entries differently.
        self.len() == other.len()
            && self
                .iter()
                .all(|(address, transfers)| other.get(address) == Some(transfers))
    }
}

impl Eq for AddressTransfers {}
