//! Omitted outputs that no block has spent yet, kept in memory by the writer.
//!
//! [`LiveOutputs`] is a bounded cache in front of the `spentness_journal`
//! column family. Every omitted output that its own block does not spend is
//! also written to the journal in that block's batch, so the map can drop
//! entries at any time: a spend that misses it, after an eviction or a
//! restart, reads the journal.
//!
//! Ported from `zakura/feat/spentness-writer` (`a909419a59`, `spentness/live.rs`),
//! with a byte budget in place of an entry count, and no lock: only the block
//! write task owns the map.

use std::collections::{BTreeMap, HashMap};

use zebra_chain::{block::Height, transparent};

use crate::service::finalized_state::disk_format::OutputLocation;

/// The default memory budget of the live map.
#[cfg(not(test))]
pub(super) const LIVE_OUTPUT_BYTES: usize = 256 * 1024 * 1024;

/// Tests use a tiny budget, so their spends also exercise the journal.
#[cfg(test)]
pub(super) const LIVE_OUTPUT_BYTES: usize = 8 * ENTRY_OVERHEAD_BYTES;

/// The estimated fixed cost of one entry: the outpoint key, the location, the
/// value, the UTXO's other fields, and the hash map's own overhead.
const ENTRY_OVERHEAD_BYTES: usize = 128;

/// Eviction drops whole buckets of this many creation heights, oldest first.
const BUCKET_HEIGHTS: u32 = 1_000;

/// One omitted output, with the location the writer needs for its spend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LiveOutput {
    /// The output's location, which also gives its height and whether it is a coinbase output.
    pub(super) location: OutputLocation,
    /// The output, as the UTXO set would have stored it.
    pub(super) utxo: transparent::Utxo,
}

impl LiveOutput {
    fn bytes(&self) -> usize {
        ENTRY_OVERHEAD_BYTES + self.utxo.output.lock_script.as_raw_bytes().len()
    }

    fn bucket(&self) -> u32 {
        bucket(self.location.height())
    }
}

/// Omitted outputs in memory, with a bounded byte size.
#[derive(Debug)]
pub(super) struct LiveOutputs {
    entries: HashMap<transparent::OutPoint, LiveOutput>,
    /// Entry counts and bytes by creation bucket, so eviction can pick a
    /// cutoff without a scan.
    buckets: BTreeMap<u32, (usize, usize)>,
    bytes: usize,
    budget: usize,
}

impl LiveOutputs {
    pub(super) fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            buckets: BTreeMap::new(),
            bytes: 0,
            budget,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn insert(&mut self, outpoint: transparent::OutPoint, output: LiveOutput) {
        let (bucket, bytes) = (output.bucket(), output.bytes());
        if let Some(old) = self.entries.insert(outpoint, output) {
            self.forget(&old);
        }
        let entry = self.buckets.entry(bucket).or_default();
        entry.0 += 1;
        entry.1 += bytes;
        self.bytes += bytes;
    }

    pub(super) fn take(&mut self, outpoint: &transparent::OutPoint) -> Option<LiveOutput> {
        let output = self.entries.remove(outpoint)?;
        self.forget(&output);
        Some(output)
    }

    pub(super) fn clear(&mut self) {
        self.entries = HashMap::new();
        self.buckets = BTreeMap::new();
        self.bytes = 0;
    }

    /// Removes `output`'s accounting, after its entry left the map.
    fn forget(&mut self, output: &LiveOutput) {
        let (bucket, bytes) = (output.bucket(), output.bytes());
        self.bytes -= bytes;
        if let Some(entry) = self.buckets.get_mut(&bucket) {
            entry.0 -= 1;
            entry.1 -= bytes;
            if entry.0 == 0 {
                self.buckets.remove(&bucket);
            }
        }
    }

    /// Drops the oldest buckets until a quarter of the budget is free.
    ///
    /// Returns the number of evicted entries. The journal still holds them.
    pub(super) fn evict_if_full(&mut self) -> usize {
        if self.bytes <= self.budget {
            return 0;
        }
        let target = self.budget - self.budget / 4;
        let mut remaining = self.bytes;
        let mut cutoff = 0;
        for (bucket, (_, bytes)) in &self.buckets {
            if remaining <= target {
                break;
            }
            remaining -= bytes;
            cutoff = bucket + 1;
        }
        let before = self.entries.len();
        self.entries.retain(|_, output| output.bucket() >= cutoff);
        self.buckets = self.buckets.split_off(&cutoff);
        self.bytes = remaining;
        before - self.entries.len()
    }
}

fn bucket(height: Height) -> u32 {
    height.0 / BUCKET_HEIGHTS
}

#[cfg(test)]
mod tests {
    use zebra_chain::{amount::Amount, transaction, transparent::Script};

    use super::*;

    fn output(height: u32) -> LiveOutput {
        LiveOutput {
            location: OutputLocation::from_usize(Height(height), 1, 0),
            utxo: transparent::Utxo::new(
                transparent::Output {
                    value: Amount::try_from(7).expect("7 is a valid amount"),
                    lock_script: Script::new(&[]),
                },
                Height(height),
                false,
            ),
        }
    }

    fn outpoint(n: u8) -> transparent::OutPoint {
        transparent::OutPoint {
            hash: transaction::Hash([n; 32]),
            index: 0,
        }
    }

    #[test]
    fn take_returns_each_entry_once() {
        let mut live = LiveOutputs::new(LIVE_OUTPUT_BYTES);
        live.insert(outpoint(1), output(10));
        assert_eq!(live.take(&outpoint(1)), Some(output(10)));
        assert_eq!(live.take(&outpoint(1)), None);
        assert_eq!(live.len(), 0);
        assert_eq!(live.bytes, 0);
        assert!(live.buckets.is_empty());
    }

    #[test]
    fn reinsertion_counts_once() {
        let mut live = LiveOutputs::new(LIVE_OUTPUT_BYTES);
        live.insert(outpoint(1), output(10));
        live.insert(outpoint(1), output(10));
        assert_eq!(live.len(), 1);
        assert_eq!(live.bytes, ENTRY_OVERHEAD_BYTES);
        assert_eq!(live.buckets.get(&0), Some(&(1, ENTRY_OVERHEAD_BYTES)));
    }

    #[test]
    fn eviction_drops_the_oldest_buckets() {
        // Room for four entries: the fifth one overflows the budget.
        let mut live = LiveOutputs::new(4 * ENTRY_OVERHEAD_BYTES);
        for (n, height) in [(1, 10), (2, 1_500), (3, 2_500), (4, 3_500), (5, 3_600)] {
            live.insert(outpoint(n), output(height));
        }
        assert_eq!(live.len(), 5);
        // A quarter of the budget must be free afterwards: three entries remain at most.
        assert_eq!(live.evict_if_full(), 2);
        assert!(live.take(&outpoint(1)).is_none());
        assert!(live.take(&outpoint(2)).is_none());
        assert!(live.take(&outpoint(3)).is_some());
        assert_eq!(live.evict_if_full(), 0);
        assert_eq!(live.len(), 2);
        assert_eq!(live.bytes, 2 * ENTRY_OVERHEAD_BYTES);
    }
}
