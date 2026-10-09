//! Created-output membership shared between non-finalized chain snapshots.

use std::{
    collections::{hash_map::RandomState, HashMap},
    hash::BuildHasher,
    sync::Arc,
};

use zebra_chain::transparent::{OrderedUtxo, OutPoint};

#[cfg(test)]
mod tests;

// Bound snapshot cloning to 256 Arc increments. Writes copy only touched partitions.
const PARTITIONS: usize = 256;
type OutputMap = HashMap<OutPoint, Arc<OrderedUtxo>>;

/// A copy-on-write output index. Clones share storage but keep independent membership.
/// Spent status still belongs to the chain's separate spent-output index.
#[derive(Clone, Debug)]
pub(crate) struct CreatedUtxos {
    // Clones must keep this seed so each outpoint stays in the same partition.
    hash: RandomState,
    partitions: Vec<Arc<OutputMap>>,
    len: usize,
}

impl Default for CreatedUtxos {
    fn default() -> Self {
        Self {
            hash: RandomState::new(),
            partitions: (0..PARTITIONS).map(|_| Arc::new(HashMap::new())).collect(),
            len: 0,
        }
    }
}

impl CreatedUtxos {
    fn partition(&self, key: &OutPoint) -> usize {
        // Use a randomized hash rather than attacker-controlled outpoint bytes.
        let mask = u64::try_from(PARTITIONS - 1).expect("the partition count fits in u64");
        usize::try_from(self.hash.hash_one(key) & mask).expect("a partition index fits in usize")
    }

    pub(crate) fn get(&self, key: &OutPoint) -> Option<&Arc<OrderedUtxo>> {
        self.partitions[self.partition(key)].get(key)
    }

    pub(crate) fn insert(
        &mut self,
        key: OutPoint,
        value: Arc<OrderedUtxo>,
    ) -> Option<Arc<OrderedUtxo>> {
        let partition = self.partition(&key);
        let previous = Arc::make_mut(&mut self.partitions[partition]).insert(key, value);
        if previous.is_none() {
            self.len += 1;
        }
        previous
    }

    pub(crate) fn remove(&mut self, key: &OutPoint) -> Option<Arc<OrderedUtxo>> {
        let partition = self.partition(key);
        let removed = Arc::make_mut(&mut self.partitions[partition]).remove(key);
        if removed.is_some() {
            self.len -= 1;
        }
        removed
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&OutPoint, &Arc<OrderedUtxo>)> {
        self.partitions
            .iter()
            .flat_map(|partition| partition.iter())
    }

    #[cfg(test)]
    pub(crate) fn contains_key(&self, key: &OutPoint) -> bool {
        self.get(key).is_some()
    }
}

impl PartialEq for CreatedUtxos {
    fn eq(&self, other: &Self) -> bool {
        // Independently built indexes can partition the same entries differently.
        self.len == other.len
            && self
                .iter()
                .all(|(key, value)| other.get(key) == Some(value))
    }
}

impl Eq for CreatedUtxos {}

#[cfg(test)]
impl std::ops::Index<&OutPoint> for CreatedUtxos {
    type Output = Arc<OrderedUtxo>;

    fn index(&self, key: &OutPoint) -> &Self::Output {
        self.get(key)
            .expect("indexed outpoints were inserted by the test")
    }
}
