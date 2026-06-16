//! Note-commitment-tree fetch lookahead for the known-hash IBD engine.
//!
//! In snapshot-consume (assumeUTXO) mode the engine does not fold note
//! commitments; instead it downloads each updating height's sapling/orchard
//! tree frontier, verifies it against the chunk's recorded root, and hands it
//! to the state's "tree supplied by download" commit path (design doc
//! `p2p-snapshot-distribution.md` §3.2).
//!
//! Trees must arrive *before* their block reaches the commit stage, so the
//! lookahead window for trees is deeper than the block-fetch window: this
//! module schedules per-height tree fetches a configurable margin ahead of the
//! commit frontier, and holds the fetched+verified trees in a bounded buffer
//! keyed by height until their block commits. If a tree has not arrived when
//! its block is ready the commit falls back to folding (correct, just slower),
//! so the lookahead is a throughput optimization, never a correctness
//! requirement.
//!
//! Only the ~7% of heights that actually update a tree need a fetch: the
//! scheduler keys its requests off the chunk's sparse updating-height list
//! ([`HashSource::tree_updates_in`](super::engine::HashSource::tree_updates_in)),
//! so non-updating heights cost nothing.
//!
//! The buffer is bounded two ways — a height-count cap (the configured margin,
//! clamped to [`TREE_LOOKAHEAD_MAX`]) and a byte cap
//! ([`TREE_BUFFER_MAX_BYTES`]) — so a node never fetches unboundedly far ahead
//! or holds an unbounded number of verified trees in RAM. Scheduling and the
//! buffer are pure bookkeeping over heights and byte counts, with no I/O, so
//! the whole module is unit-testable without a live chain or peers.

use std::collections::{BTreeMap, BTreeSet};

use zebra_chain::block;
use zebra_network::ShieldedPool;

/// The hard ceiling on how many heights ahead of the commit frontier the tree
/// lookahead schedules fetches, regardless of the configured margin.
///
/// Bounds both the per-loop scheduling scan and the in-flight tree fetch count.
/// Trees update at only ~7% of heights, so a margin of this many *block* heights
/// schedules far fewer than this many tree fetches in practice; the ceiling only
/// binds if a misconfiguration sets an enormous margin.
pub const TREE_LOOKAHEAD_MAX: u32 = 16_384;

/// The default tree lookahead margin, in block heights ahead of the commit
/// frontier (`sync.known_hash_tree_lookahead`).
///
/// Deeper than the block fetch-ahead in large-block eras so a height's tree is
/// already downloaded and verified by the time its block commits, while staying
/// well under [`TREE_LOOKAHEAD_MAX`]. At ~7% updating heights this schedules a
/// few hundred tree fetches at most.
pub const TREE_LOOKAHEAD_DEFAULT: u32 = 4_096;

/// The byte cap on the verified-tree buffer.
///
/// Sapling/orchard frontier serializations are small (a few KiB each), so this
/// holds thousands of buffered trees — more than [`TREE_LOOKAHEAD_MAX`] heights
/// could ever supply — while still bounding worst-case RAM if a future tree
/// serialization grows. Scheduling stops issuing new fetches once the buffer
/// plus in-flight reservation would exceed this.
pub const TREE_BUFFER_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// The verified trees for one height: at most one sapling and one orchard tree.
///
/// A height may update only one pool (the sparse lists are independent), so
/// either slot may be empty even after the height's fetch completes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeightTrees {
    /// The verified sapling tree at this height, if it updated and arrived.
    pub sapling: Option<Vec<u8>>,

    /// The verified orchard tree at this height, if it updated and arrived.
    pub orchard: Option<Vec<u8>>,
}

impl HeightTrees {
    /// The total buffered bytes across both pools.
    fn bytes(&self) -> u64 {
        let sapling = self.sapling.as_ref().map_or(0, Vec::len) as u64;
        let orchard = self.orchard.as_ref().map_or(0, Vec::len) as u64;
        sapling + orchard
    }

    /// Records a verified tree for `pool`, returning the byte delta added to the
    /// buffer (zero if the pool already held a tree, which is then overwritten).
    fn insert(&mut self, pool: ShieldedPool, bytes: Vec<u8>) -> u64 {
        let slot = match pool {
            ShieldedPool::Sapling => &mut self.sapling,
            ShieldedPool::Orchard => &mut self.orchard,
        };
        let old = slot.as_ref().map_or(0, Vec::len) as u64;
        let new = bytes.len() as u64;
        *slot = Some(bytes);
        // Overwrites are not expected (one fetch per pool per height), but
        // account precisely if one happens: the net change is new - old.
        new.saturating_sub(old)
    }
}

/// One scheduled tree fetch: a pool at an absolute height.
///
/// Ordered height-first (then by pool), so the in-flight [`BTreeSet`] and the
/// schedule output are in ascending height order, and
/// [`TreeLookahead::evict_below`] can split the set at a height boundary.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TreeFetch {
    /// The absolute block height whose tree to fetch.
    pub height: block::Height,

    /// Which shielded pool to fetch.
    pub pool: ShieldedPool,
}

/// A stable ordinal for a pool, so [`TreeFetch`] can order height-first without
/// requiring [`ShieldedPool`] to be `Ord`.
fn pool_ordinal(pool: ShieldedPool) -> u8 {
    match pool {
        ShieldedPool::Sapling => 0,
        ShieldedPool::Orchard => 1,
    }
}

impl Ord for TreeFetch {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.height
            .cmp(&other.height)
            .then_with(|| pool_ordinal(self.pool).cmp(&pool_ordinal(other.pool)))
    }
}

impl PartialOrd for TreeFetch {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The bounded tree-fetch lookahead: a buffer of verified trees keyed by
/// height, the set of in-flight `(height, pool)` fetches, and the byte
/// accounting for both bounds.
///
/// Owned and driven by the engine loop; pure bookkeeping with no I/O.
#[derive(Debug, Default)]
pub struct TreeLookahead {
    /// Fetched, verified trees awaiting their block's commit, keyed by absolute
    /// height. Evicted on take (commit) or when the frontier passes the height.
    buffer: BTreeMap<block::Height, HeightTrees>,

    /// In-flight `(height, pool)` tree fetches, so the scheduler never
    /// double-issues and the byte reservation can bound issuance.
    in_flight: BTreeSet<TreeFetch>,

    /// The exact bytes of buffered trees (the byte-cap accounting target).
    buffer_bytes: u64,
}

impl TreeLookahead {
    /// A fresh, empty lookahead.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of heights with at least one buffered tree.
    pub fn buffered_heights(&self) -> usize {
        self.buffer.len()
    }

    /// The number of in-flight tree fetches.
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    /// The exact buffered-tree bytes.
    pub fn buffer_bytes(&self) -> u64 {
        self.buffer_bytes
    }

    /// Whether a `(height, pool)` fetch is already in flight.
    pub fn is_in_flight(&self, fetch: TreeFetch) -> bool {
        self.in_flight.contains(&fetch)
    }

    /// Whether the buffer already holds the tree for `(height, pool)`.
    fn is_buffered(&self, fetch: TreeFetch) -> bool {
        match self.buffer.get(&fetch.height) {
            None => false,
            Some(trees) => match fetch.pool {
                ShieldedPool::Sapling => trees.sapling.is_some(),
                ShieldedPool::Orchard => trees.orchard.is_some(),
            },
        }
    }

    /// Selects the next batch of tree fetches to issue, ahead of the commit
    /// frontier (design doc §3.2 "Tree lookahead").
    ///
    /// `updates` is the chunk's sparse updating-height list within the lookahead
    /// window `[frontier, frontier + margin]`, as `(height, pool)` pairs in
    /// ascending order (only the heights that actually update a tree, so
    /// non-updating heights cost nothing). `margin` is the configured block-height
    /// margin, already clamped to [`TREE_LOOKAHEAD_MAX`] by the caller.
    ///
    /// Returns every pair that is not already buffered or in flight, lowest
    /// height first, stopping once the buffer plus the in-flight byte reservation
    /// would exceed [`TREE_BUFFER_MAX_BYTES`]. Each returned fetch is recorded
    /// in-flight, so a fetch is never issued twice. The byte reservation per
    /// in-flight fetch is estimated from the largest tree seen so far (or a small
    /// floor before any tree has arrived), keeping the bound conservative without
    /// knowing the exact size until a tree arrives.
    pub fn schedule(&mut self, updates: impl IntoIterator<Item = TreeFetch>) -> Vec<TreeFetch> {
        let mut issue = Vec::new();

        // A conservative per-fetch byte reservation: the largest tree seen so
        // far, or a small floor before any has arrived. This keeps the byte cap
        // honest for in-flight (not-yet-sized) fetches.
        let reserve_per_fetch = self.reserve_per_fetch();

        for fetch in updates {
            if self.is_buffered(fetch) || self.is_in_flight(fetch) {
                continue;
            }

            // Bound the in-flight count to the same ceiling as the window, so the
            // scheduling scan and the staged-future count stay bounded even if a
            // misconfiguration sets an enormous margin.
            if self.in_flight.len() + issue.len() >= TREE_LOOKAHEAD_MAX as usize {
                break;
            }

            // Stop issuing once the buffer plus the reservation for everything
            // in flight (existing + this batch) would exceed the byte cap.
            let reserved = (self.in_flight.len() + issue.len() + 1) as u64 * reserve_per_fetch;
            if self.buffer_bytes + reserved > TREE_BUFFER_MAX_BYTES {
                break;
            }

            issue.push(fetch);
        }

        for fetch in &issue {
            self.in_flight.insert(*fetch);
        }

        issue
    }

    /// The conservative per-in-flight-fetch byte reservation: the largest
    /// buffered tree's total height bytes, or a 64 KiB floor before any tree has
    /// arrived (frontier serializations are a few KiB, so this floor is already
    /// generous).
    fn reserve_per_fetch(&self) -> u64 {
        const FLOOR: u64 = 64 * 1024;
        self.buffer
            .values()
            .map(HeightTrees::bytes)
            .max()
            .unwrap_or(0)
            .max(FLOOR)
    }

    /// Records a fetched, verified tree, clearing its in-flight marker and
    /// adding it to the buffer.
    ///
    /// A tree for a height the frontier has already passed (no longer schedulable)
    /// is dropped rather than buffered, so a late arrival never grows the buffer
    /// past the frontier.
    pub fn on_fetched(&mut self, fetch: TreeFetch, bytes: Vec<u8>, frontier: block::Height) {
        self.in_flight.remove(&fetch);

        if fetch.height < frontier {
            return;
        }

        let entry = self.buffer.entry(fetch.height).or_default();
        let delta = entry.insert(fetch.pool, bytes);
        self.buffer_bytes = self.buffer_bytes.saturating_add(delta);
    }

    /// Clears the in-flight marker for a tree fetch that failed, so the
    /// scheduler may reissue it on the next pass.
    pub fn on_failed(&mut self, fetch: TreeFetch) {
        self.in_flight.remove(&fetch);
    }

    /// Removes and returns the buffered trees for `height`, for its commit (the
    /// "tree supplied by download" path).
    ///
    /// Returns `None` when no tree was buffered for the height (a non-updating
    /// height, or one whose fetch has not arrived): the caller falls back to
    /// folding.
    pub fn take(&mut self, height: block::Height) -> Option<HeightTrees> {
        let trees = self.buffer.remove(&height)?;
        self.buffer_bytes = self.buffer_bytes.saturating_sub(trees.bytes());
        Some(trees)
    }

    /// Evicts every buffered tree and in-flight marker strictly below `frontier`
    /// (the commit frontier passed them without taking them — e.g. a tree
    /// arrived after its block had already folded).
    ///
    /// Keeps the buffer bounded by the live window even when takes are missed.
    pub fn evict_below(&mut self, frontier: block::Height) {
        // Split off the kept (>= frontier) suffix; the removed prefix is dropped.
        let kept = self.buffer.split_off(&frontier);
        for trees in self.buffer.values() {
            self.buffer_bytes = self.buffer_bytes.saturating_sub(trees.bytes());
        }
        self.buffer = kept;

        self.in_flight = self.in_flight.split_off(&TreeFetch {
            height: frontier,
            pool: ShieldedPool::Sapling,
        });
    }
}

#[cfg(test)]
mod tests;
