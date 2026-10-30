//! Created-output membership shared between non-finalized chain snapshots.

use std::{collections::hash_map::RandomState, hash::BuildHasher, sync::Arc};

use hashbrown::{hash_table::Entry, HashTable};

use zebra_chain::transparent::{OrderedUtxo, OutPoint};

#[cfg(test)]
mod tests;

// Bound snapshot cloning to 256 Arc increments. Writes copy only touched partitions.
const PARTITIONS: usize = 256;
type OutputTable = HashTable<(OutPoint, Arc<OrderedUtxo>)>;

/// Selects a partition from a key's seeded hash, so each lookup hashes the key once.
///
/// Bits 32..40 are independent of the bits hashbrown uses for buckets (the low bits of tables
/// under 2^32 buckets) and control tags (the top seven), so keys spread evenly in each partition.
pub(super) fn partition(hash: u64) -> usize {
    let mask = u64::try_from(PARTITIONS - 1).expect("the partition count fits in u64");
    usize::try_from((hash >> 32) & mask).expect("a partition index fits in usize")
}

/// A copy-on-write output index with independent membership in each snapshot.
/// Spent status belongs to the chain's separate spent-output index.
#[derive(Clone, Debug)]
pub(crate) struct CreatedUtxos {
    // Clones keep this seed so each outpoint stays in the same partition.
    hash: RandomState,
    partitions: Vec<Arc<OutputTable>>,
    len: usize,
}

impl Default for CreatedUtxos {
    fn default() -> Self {
        Self {
            hash: RandomState::new(),
            partitions: (0..PARTITIONS).map(|_| Arc::default()).collect(),
            len: 0,
        }
    }
}

impl CreatedUtxos {
    #[cfg(test)]
    fn partition(&self, key: &OutPoint) -> usize {
        partition(self.hash.hash_one(key))
    }

    pub(crate) fn get(&self, key: &OutPoint) -> Option<&Arc<OrderedUtxo>> {
        let hash = self.hash.hash_one(key);
        self.partitions[partition(hash)]
            .find(hash, |(stored, _)| stored == key)
            .map(|(_, value)| value)
    }

    pub(crate) fn insert(
        &mut self,
        key: OutPoint,
        value: Arc<OrderedUtxo>,
    ) -> Option<Arc<OrderedUtxo>> {
        let hash = self.hash.hash_one(key);
        let state = &self.hash;
        let table = Arc::make_mut(&mut self.partitions[partition(hash)]);
        match table.entry(
            hash,
            |(stored, _)| *stored == key,
            |(k, _)| state.hash_one(k),
        ) {
            Entry::Occupied(mut entry) => Some(std::mem::replace(&mut entry.get_mut().1, value)),
            Entry::Vacant(entry) => {
                entry.insert((key, value));
                self.len += 1;
                None
            }
        }
    }

    pub(crate) fn remove(&mut self, key: &OutPoint) -> Option<Arc<OrderedUtxo>> {
        let hash = self.hash.hash_one(key);
        let table = Arc::make_mut(&mut self.partitions[partition(hash)]);
        let (_, removed) = table
            .find_entry(hash, |(stored, _)| stored == key)
            .ok()?
            .remove()
            .0;
        self.len -= 1;
        Some(removed)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&OutPoint, &Arc<OrderedUtxo>)> {
        self.partitions
            .iter()
            .flat_map(|partition| partition.iter().map(|(key, value)| (key, value)))
    }

    #[cfg(test)]
    pub(crate) fn contains_key(&self, key: &OutPoint) -> bool {
        self.get(key).is_some()
    }
}

impl PartialEq for CreatedUtxos {
    fn eq(&self, other: &Self) -> bool {
        // Independently built indexes can partition equal entries differently.
        self.len == other.len
            && self
                .iter()
                .all(|(key, value)| other.get(key) == Some(value))
    }
}

impl Eq for CreatedUtxos {}
