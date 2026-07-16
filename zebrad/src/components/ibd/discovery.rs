//! A discovery-fed [`HashSource`] for the generic IBD engine.
//!
//! Where [`KnownHashList`](zebra_chain::parameters::known_hashes::KnownHashList)
//! and the CF-backed snapshot source pin a fixed hash per height, the
//! [`DiscoverySource`] grows as the syncer's `ObtainTips`/`ExtendTips` crawl
//! discovers new tentative hashes (design doc `known-hash-ibd.md` §17): the
//! crawl stays in `ChainSync` and *feeds* the source through a
//! [`DiscoveryFeed`] handle, and the engine drives its window, weighted-fetch,
//! gap-hedge, and commit-pipeline machinery over the growing range exactly as
//! it does over a pinned list.
//!
//! The hashes are **tentative**: they come from peers, not a reviewed
//! constant, so the stage-2 [`SemanticCommit`](super::semantic::SemanticCommit)
//! runs full semantic and contextual validation on every block. A wrong
//! tentative sequence (for example, two peers' forks interleaved by the crawl)
//! fails contextual validation and surfaces as an engine error; the syncer
//! then restarts the cycle from the state tip, converging exactly as the
//! legacy syncer's restart loop did. The engine-side surgical window reorg
//! (dropping only the slots above a fork point) is a recorded follow-up
//! optimization; restart-based recovery is the correctness baseline.
//!
//! The source is single-owner (the engine task); the syncer's feed handle
//! sends events over an unbounded channel that the engine drains at each
//! loop pass, so there is no shared mutable state.

use std::collections::VecDeque;

use tokio::sync::{mpsc, watch};

use zebra_chain::block;

use super::engine::HashSource;
use crate::BoxError;

/// The number of hashes below which the source signals the syncer to extend
/// the tips.
///
/// Sized so the crawl runs well before the engine's fetch frontier goes idle:
/// an `ExtendTips` round returns up to ~500 hashes per tip, and a round trip
/// takes well under the time the engine needs to drain this many blocks.
pub const DISCOVERY_LOW_WATER_MARK: u64 = 200;

/// A crawl event feeding the [`DiscoverySource`].
#[derive(Debug)]
enum DiscoveryEvent {
    /// Append these hashes above the source's current maximum height, in
    /// chain order.
    Extend(Vec<block::Hash>),

    /// The crawl has run out of prospective tips: no more hashes are coming
    /// this cycle, so the engine completes once the range drains.
    Finish,
}

/// The syncer's handle for feeding a [`DiscoverySource`].
///
/// Dropping the feed without [`finish`](Self::finish) also finishes the
/// source (the closed channel is drained as a finish), so an aborted crawl
/// can't hang the engine.
#[derive(Debug)]
pub struct DiscoveryFeed {
    /// Sends crawl events to the source.
    events: mpsc::UnboundedSender<DiscoveryEvent>,

    /// The cumulative count of hashes the source has accounted for — released
    /// (committed and dropped from the engine's window) or skipped as a
    /// crawl-overlap duplicate — published by the source.
    released: watch::Receiver<u64>,

    /// The cumulative count of hashes this feed has sent.
    ///
    /// The outstanding (fed but not yet accounted-for) count is `fed - released`.
    /// Tracked on the feed side so it updates the instant [`extend`](Self::extend)
    /// queues hashes, rather than only when the engine next drains the event
    /// channel — otherwise a stale released count would let the syncer crawl
    /// far past [`DISCOVERY_LOW_WATER_MARK`] before the engine catches up.
    fed: u64,
}

impl DiscoveryFeed {
    /// Appends `hashes` above the source's current maximum height, in chain
    /// order. A send after the engine stopped is silently dropped.
    pub fn extend(&mut self, hashes: Vec<block::Hash>) {
        if !hashes.is_empty() {
            // usize -> u64 is lossless on every supported platform. This can
            // slightly over-count when the source skips a crawl-overlap prefix,
            // which only makes `wants_more_hashes` a touch more conservative.
            self.fed = self.fed.saturating_add(hashes.len() as u64);
            let _ = self.events.send(DiscoveryEvent::Extend(hashes));
        }
    }

    /// Tells the source no more hashes are coming this cycle, letting the
    /// engine complete once the appended range drains.
    pub fn finish(&self) {
        let _ = self.events.send(DiscoveryEvent::Finish);
    }

    /// Waits until the outstanding (fed but not yet committed) hash count
    /// drops to [`DISCOVERY_LOW_WATER_MARK`] or below, so the syncer extends
    /// the tips just ahead of the engine draining them.
    ///
    /// Returns immediately if the source (engine) is gone.
    pub async fn wants_more_hashes(&mut self) {
        while self.fed.saturating_sub(*self.released.borrow()) > DISCOVERY_LOW_WATER_MARK {
            if self.released.changed().await.is_err() {
                return;
            }
        }
    }
}

/// A [`HashSource`] over tentative hashes discovered by the syncer's tip
/// crawl.
///
/// Owned and driven by the engine; fed through the paired [`DiscoveryFeed`].
#[derive(Debug)]
pub struct DiscoverySource {
    /// The height of `hashes[0]`: the lowest height the engine has not
    /// committed yet.
    base: block::Height,

    /// The hash anchoring the range: the best hash known for `base - 1`.
    ///
    /// Starts as the state tip hash and advances as hashes are released, so
    /// the engine's frontier parent pin (`list[base - 1]`) always resolves.
    /// When the crawl actually discovered a side chain (a fork below the
    /// tip), the initial anchor is not the true parent of `hashes[0]` — that
    /// is fine, because the semantic commit stage ignores the parent pin: the
    /// verifier derives every parent from the block itself.
    anchor: block::Hash,

    /// The tentative hashes for `base..`, in height order.
    hashes: VecDeque<block::Hash>,

    /// Crawl events from the [`DiscoveryFeed`].
    events: mpsc::UnboundedReceiver<DiscoveryEvent>,

    /// Whether the crawl finished (or the feed was dropped): the range no
    /// longer grows, so draining it completes the engine.
    finished: bool,

    /// The cumulative count of hashes accounted for: released by the engine
    /// (committed and dropped from its window) plus crawl-overlap duplicates
    /// skipped by [`apply`](Self::apply). Published to the feed's low-water gate.
    released: u64,

    /// Publishes [`released`](Self::released) to the feed.
    released_tx: watch::Sender<u64>,
}

impl DiscoverySource {
    /// Returns a source whose first hash will be for `base` (the lowest
    /// uncommitted height), anchored on `anchor` (the hash committed at
    /// `base - 1`, normally the state tip hash), and the feed that grows it.
    ///
    /// `base` is always at least [`Height(1)`](block::Height): the genesis
    /// block is committed before the sync cycles start, so the lowest
    /// uncommitted height is never genesis. [`max_height`](HashSource::max_height)
    /// relies on this to represent an empty range as `base - 1`.
    pub fn new(base: block::Height, anchor: block::Hash) -> (DiscoveryFeed, Self) {
        debug_assert!(
            base.0 >= 1,
            "the genesis block is committed before discovery, so base is never height 0",
        );

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (released_tx, released_rx) = watch::channel(0);

        (
            DiscoveryFeed {
                events: events_tx,
                released: released_rx,
                fed: 0,
            },
            Self {
                base,
                anchor,
                hashes: VecDeque::new(),
                events: events_rx,
                finished: false,
                released: 0,
                released_tx,
            },
        )
    }

    /// Applies every already-queued crawl event, without waiting.
    fn drain_events(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(event) => self.apply(event),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    // The syncer dropped the feed: treat as a finish so the
                    // engine drains what it has and completes.
                    self.finished = true;
                    break;
                }
            }
        }

        // `apply` may have bumped `released` for skipped duplicates.
        self.publish_released();
    }

    /// Applies one crawl event.
    fn apply(&mut self, event: DiscoveryEvent) {
        match event {
            DiscoveryEvent::Extend(new_hashes) => {
                // A hash that duplicates the current tail is a crawl overlap
                // (two peers answering the same locator); skip the overlapping
                // prefix rather than assigning the same block two heights.
                let mut new_hashes = new_hashes.into_iter();
                if let Some(tail) = self.hashes.back().copied() {
                    let mut peeked = new_hashes.clone();
                    if peeked.next() == Some(tail) {
                        new_hashes = peeked;
                        // The feed counted the skipped hash in `fed`, so count
                        // it as released too — otherwise `fed - released`
                        // over-counts the outstanding hashes by one per skip,
                        // and after enough skips the feed's low-water gate
                        // (`wants_more_hashes`) would stop crawling while the
                        // engine still waits for more, deadlocking the cycle.
                        self.released = self.released.saturating_add(1);
                    }
                }

                self.hashes.extend(new_hashes);
            }
            DiscoveryEvent::Finish => self.finished = true,
        }
    }

    /// Publishes the cumulative released count to the feed's low-water gate.
    fn publish_released(&self) {
        self.released_tx.send_if_modified(|current| {
            if *current != self.released {
                *current = self.released;
                true
            } else {
                false
            }
        });
    }
}

impl HashSource for DiscoverySource {
    fn max_height(&self) -> block::Height {
        // With no hashes, the maximum is just below `base`, so the engine sees
        // an empty range. `new` asserts `base >= 1`, so `base - 1` never
        // underflows here; `len` fits u32 because the window (and so the fed
        // range the engine holds) is bounded far below u32::MAX.
        block::Height(
            self.base
                .0
                .saturating_add(self.hashes.len() as u32)
                .saturating_sub(1),
        )
    }

    fn hash(&mut self, height: block::Height) -> Result<Option<block::Hash>, BoxError> {
        // The engine pins the frontier block's parent with `list[base - 1]`,
        // which the anchor serves.
        if height.0.checked_add(1) == Some(self.base.0) {
            return Ok(Some(self.anchor));
        }

        let index = match height.0.checked_sub(self.base.0) {
            // `index` is a VecDeque offset, which is always well below usize::MAX.
            Some(index) => index as usize,
            // Below the anchor: already committed and released.
            None => return Ok(None),
        };

        Ok(self.hashes.get(index).copied())
    }

    fn release_below(&mut self, height: block::Height) {
        while self.base < height {
            match self.hashes.pop_front() {
                // The released hash becomes the anchor for the new base.
                Some(hash) => self.anchor = hash,
                None => break,
            }
            self.base = block::Height(self.base.0.saturating_add(1));
            self.released = self.released.saturating_add(1);
        }

        self.publish_released();
    }

    fn ensure_covers(
        &mut self,
        _start: block::Height,
        _end: block::Height,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), BoxError>> + Send + '_>>
    {
        // Nothing to prime from disk; just fold in whatever the crawl has
        // already queued so the refill pass sees the freshest range.
        Box::pin(async move {
            self.drain_events();
            Ok(())
        })
    }

    fn is_final(&self) -> bool {
        self.finished
    }

    fn wait_for_growth(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + '_>> {
        Box::pin(async move {
            // "Growth" means the range extends above what the engine has
            // already seen — not merely that `hashes` is non-empty. The engine
            // calls `release_below(base - 1)`, deliberately retaining the
            // committed parent as `hashes[0]`, so a fully-committed source is
            // non-empty; keying off non-emptiness would make this return `true`
            // forever and spin the engine's completion loop.
            let start = self.max_height();
            loop {
                self.drain_events();
                if self.max_height() > start {
                    return true;
                }
                if self.finished {
                    return false;
                }

                match self.events.recv().await {
                    Some(event) => self.apply(event),
                    None => {
                        self.finished = true;
                        return false;
                    }
                }
            }
        })
    }

    fn invalidate_above(&mut self, height: block::Height) {
        // `keep` counts the hashes at or below `height`, which is a VecDeque
        // length and so always well below usize::MAX.
        let keep = height.0.saturating_add(1).saturating_sub(self.base.0) as usize;
        self.hashes.truncate(keep);
    }
}

#[cfg(test)]
mod tests;
