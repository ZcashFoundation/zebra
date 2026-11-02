//! Transparent address histories shared between non-finalized chain snapshots.

use std::{collections::hash_map::RandomState, hash::BuildHasher, sync::Arc};

use hashbrown::HashTable;

use zebra_chain::transparent::Address;

use super::{chain::index::TransparentTransfers, created_utxos::partition};

#[cfg(test)]
mod tests;

// Bound snapshot cloning to 256 Arc increments. Writes copy only touched partitions.
const PARTITIONS: usize = 256;
type AddressTable = HashTable<(Address, Arc<TransparentTransfers>)>;

/// A copy-on-write transparent address index.
/// Updates copy touched partitions and histories, but share created-output entries at other heights.
#[derive(Clone, Debug)]
pub(crate) struct AddressTransfers {
    // Clones keep this seed so each address stays in the same partition.
    hash: RandomState,
    partitions: Vec<Arc<AddressTable>>,
}

impl Default for AddressTransfers {
    fn default() -> Self {
        Self {
            hash: RandomState::new(),
            partitions: (0..PARTITIONS).map(|_| Arc::default()).collect(),
        }
    }
}

impl AddressTransfers {
    #[cfg(test)]
    fn partition(&self, address: &Address) -> usize {
        partition(self.hash.hash_one(address))
    }

    pub(crate) fn get(&self, address: &Address) -> Option<&TransparentTransfers> {
        let hash = self.hash.hash_one(address);
        self.partitions[partition(hash)]
            .find(hash, |(stored, _)| stored == address)
            .map(|(_, transfers)| transfers.as_ref())
    }

    pub(crate) fn get_or_insert_mut(&mut self, address: Address) -> &mut TransparentTransfers {
        let hash = self.hash.hash_one(address);
        let state = &self.hash;
        let entry = Arc::make_mut(&mut self.partitions[partition(hash)])
            .entry(
                hash,
                |(stored, _)| *stored == address,
                |(stored, _)| state.hash_one(stored),
            )
            .or_insert_with(|| (address, Arc::default()))
            .into_mut();
        Arc::make_mut(&mut entry.1)
    }

    pub(crate) fn get_mut(&mut self, address: &Address) -> Option<&mut TransparentTransfers> {
        let hash = self.hash.hash_one(address);
        Arc::make_mut(&mut self.partitions[partition(hash)])
            .find_mut(hash, |(stored, _)| stored == address)
            .map(|(_, transfers)| Arc::make_mut(transfers))
    }

    pub(crate) fn remove(&mut self, address: &Address) -> Option<Arc<TransparentTransfers>> {
        let hash = self.hash.hash_one(address);
        let entry = Arc::make_mut(&mut self.partitions[partition(hash)])
            .find_entry(hash, |(stored, _)| stored == address)
            .ok()?;
        Some(entry.remove().0 .1)
    }

    pub(crate) fn len(&self) -> usize {
        self.partitions
            .iter()
            .map(|partition| partition.len())
            .sum()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Address, &TransparentTransfers)> {
        self.partitions.iter().flat_map(|table| {
            table
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
