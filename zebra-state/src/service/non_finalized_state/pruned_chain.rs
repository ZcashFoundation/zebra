//! A cache of recently finalized chain data, for fast in-memory reads during
//! the initial checkpoint sync.
//!
//! When the checkpoint pipeline finalizes a non-finalized chain root, the
//! root's data leaves memory and later reads of it become database point
//! reads. During the 2022–2023 transaction spam, most transparent outputs
//! are spent within a few hundred blocks of their creation — so those
//! database reads dominate the write path exactly where blocks are densest.
//!
//! [`PrunedChain`] is a chain pruned down to the one field the checkpoint
//! write path reads back: the **spendable transparent outputs** of recently
//! finalized blocks. Spend lookups consult it between the live chain and the
//! database (see `check::utxo::transparent_spend`), turning the spam era's
//! create-then-spend pattern into memory hits.
//!
//! The cache is bounded two ways: entries are removed as soon as they are
//! spent (the common case — a pinned chain can't double-spend, so a spent
//! entry can never be read again), and whole heights are evicted once they
//! fall out of the retention window
//! ([`Config::checkpoint_sync_retained_blocks`](crate::Config::checkpoint_sync_retained_blocks)).
//! Memory use is therefore proportional to the window's *unspent* outputs,
//! not to whole blocks.

use std::collections::{BTreeMap, HashMap};

use zebra_chain::{block::Height, transparent};

/// A cache of the spendable transparent outputs created by recently
/// finalized blocks (see the [module docs](self)).
///
/// Only used while the checkpoint pipeline is running: created by
/// [`NonFinalizedState::enable_pruned_chain`](super::NonFinalizedState::enable_pruned_chain)
/// and dropped at the end of the bulk-write phase.
#[derive(Clone, Debug)]
pub struct PrunedChain {
    /// The retention window, in blocks: heights more than this far below the
    /// most recently added height are evicted.
    retained_blocks: u32,

    /// The cached spendable outputs of recently finalized blocks, by
    /// outpoint.
    created_utxos: HashMap<transparent::OutPoint, transparent::OrderedUtxo>,

    /// The cached outpoints created at each retained height, for eviction.
    ///
    /// Outpoints removed from `created_utxos` when they are spent are *not*
    /// removed from this index (eviction tolerates missing entries); the
    /// index is bounded by the retention window like the cache itself.
    created_by_height: BTreeMap<Height, Vec<transparent::OutPoint>>,
}

impl PrunedChain {
    /// Returns an empty cache retaining `retained_blocks` of finalized
    /// history.
    pub fn new(retained_blocks: u32) -> Self {
        Self {
            retained_blocks,
            created_utxos: HashMap::new(),
            created_by_height: BTreeMap::new(),
        }
    }

    /// Returns the cached spendable output for `outpoint`, if it is a
    /// retained, still-unspent output of a recently finalized block.
    pub fn utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::OrderedUtxo> {
        self.created_utxos.get(outpoint).cloned()
    }

    /// Caches the still-spendable outputs of a finalized chain root at
    /// `height`, and evicts heights that have left the retention window.
    ///
    /// The caller filters the root's outputs down to the ones not already
    /// spent inside the chain, so the cache never serves an output the chain
    /// has spent.
    pub fn add_finalized_root<'a>(
        &mut self,
        height: Height,
        spendable_outputs: impl Iterator<
            Item = (&'a transparent::OutPoint, &'a transparent::OrderedUtxo),
        >,
    ) {
        let outpoints = self.created_by_height.entry(height).or_default();
        for (outpoint, utxo) in spendable_outputs {
            outpoints.push(*outpoint);
            self.created_utxos.insert(*outpoint, utxo.clone());
        }

        // Evict heights that have left the retention window. Roots finalize
        // in height order, so `height` is the newest retained height.
        let evict_through = Height(height.0.saturating_sub(self.retained_blocks));
        while let Some((&oldest, _)) = self.created_by_height.first_key_value() {
            if oldest > evict_through {
                break;
            }

            let outpoints = self
                .created_by_height
                .remove(&oldest)
                .expect("just read this key");
            for outpoint in outpoints {
                self.created_utxos.remove(&outpoint);
            }
        }
    }

    /// Removes spent outpoints from the cache.
    ///
    /// Called with each committed block's resolved spends: a spent output
    /// can never be read again (the pinned checkpoint chain has no
    /// double-spends), so dropping it immediately keeps the cache
    /// proportional to the window's unspent outputs.
    pub fn remove_spent<'a>(&mut self, spent: impl Iterator<Item = &'a transparent::OutPoint>) {
        for outpoint in spent {
            self.created_utxos.remove(outpoint);
        }
    }

    /// Returns the number of cached spendable outputs.
    #[cfg(test)]
    pub fn utxo_count(&self) -> usize {
        self.created_utxos.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use zebra_chain::{
        block,
        transparent::{self, OrderedUtxo},
    };

    /// Returns a unique outpoint and a dummy ordered output for `n`.
    fn utxo(n: u32) -> (transparent::OutPoint, OrderedUtxo) {
        let outpoint = transparent::OutPoint {
            hash: zebra_chain::transaction::Hash([n as u8; 32]),
            index: n,
        };
        let output = transparent::Output {
            value: zebra_chain::amount::Amount::try_from(1).expect("valid amount"),
            lock_script: transparent::Script::new(&[n as u8]),
        };
        let utxo = OrderedUtxo::new(output, block::Height(n), n as usize);

        (outpoint, utxo)
    }

    /// Cached outputs are readable until they are spent, then gone.
    #[test]
    fn caches_until_spent() {
        let _init_guard = zebra_test::init();

        let mut pruned = PrunedChain::new(10);
        let (outpoint, ordered_utxo) = utxo(1);

        pruned.add_finalized_root(
            block::Height(1),
            std::iter::once((&outpoint, &ordered_utxo)),
        );
        assert!(pruned.utxo(&outpoint).is_some());

        pruned.remove_spent(std::iter::once(&outpoint));
        assert!(pruned.utxo(&outpoint).is_none());
        assert_eq!(pruned.utxo_count(), 0);
    }

    /// Heights are evicted once they leave the retention window.
    #[test]
    fn evicts_outside_retention_window() {
        let _init_guard = zebra_test::init();

        let mut pruned = PrunedChain::new(5);

        let outpoints: Vec<_> = (1..=10u32)
            .map(|height| {
                let (outpoint, ordered_utxo) = utxo(height);
                pruned.add_finalized_root(
                    block::Height(height),
                    std::iter::once((&outpoint, &ordered_utxo)),
                );
                outpoint
            })
            .collect();

        // After adding height 10 with a window of 5, heights 1..=5 are
        // evicted and 6..=10 remain.
        for (height, outpoint) in (1..=10u32).zip(&outpoints) {
            assert_eq!(
                pruned.utxo(outpoint).is_some(),
                height > 5,
                "height {height} should be retained iff above the eviction bound",
            );
        }
        assert_eq!(pruned.utxo_count(), 5);
    }
}
