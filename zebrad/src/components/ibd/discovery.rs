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

    /// The source's count of hashes at or above the fetch frontier.
    remaining: watch::Receiver<u64>,
}

impl DiscoveryFeed {
    /// Appends `hashes` above the source's current maximum height, in chain
    /// order. A send after the engine stopped is silently dropped.
    pub fn extend(&self, hashes: Vec<block::Hash>) {
        if !hashes.is_empty() {
            let _ = self.events.send(DiscoveryEvent::Extend(hashes));
        }
    }

    /// Tells the source no more hashes are coming this cycle, letting the
    /// engine complete once the appended range drains.
    pub fn finish(&self) {
        let _ = self.events.send(DiscoveryEvent::Finish);
    }

    /// Waits until the source's remaining (uncommitted) hash count drops to
    /// [`DISCOVERY_LOW_WATER_MARK`] or below, so the syncer extends the tips
    /// just ahead of the engine draining them.
    ///
    /// Returns immediately if the source (engine) is gone.
    pub async fn wants_more_hashes(&mut self) {
        while *self.remaining.borrow() > DISCOVERY_LOW_WATER_MARK {
            if self.remaining.changed().await.is_err() {
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

    /// Publishes the remaining hash count to the feed after every change.
    remaining: watch::Sender<u64>,
}

impl DiscoverySource {
    /// Returns a source whose first hash will be for `base` (the lowest
    /// uncommitted height), anchored on `anchor` (the hash committed at
    /// `base - 1`, normally the state tip hash), and the feed that grows it.
    pub fn new(base: block::Height, anchor: block::Hash) -> (DiscoveryFeed, Self) {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (remaining_tx, remaining_rx) = watch::channel(0);

        (
            DiscoveryFeed {
                events: events_tx,
                remaining: remaining_rx,
            },
            Self {
                base,
                anchor,
                hashes: VecDeque::new(),
                events: events_rx,
                finished: false,
                remaining: remaining_tx,
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

        self.publish_remaining();
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
                    }
                }

                self.hashes.extend(new_hashes);
            }
            DiscoveryEvent::Finish => self.finished = true,
        }
    }

    /// Publishes the remaining hash count to the feed.
    fn publish_remaining(&self) {
        let remaining = self.hashes.len() as u64;
        self.remaining.send_if_modified(|current| {
            if *current != remaining {
                *current = remaining;
                true
            } else {
                false
            }
        });
    }
}

impl HashSource for DiscoverySource {
    fn max_height(&self) -> block::Height {
        // With no hashes, the maximum is just below `base`, so the engine
        // sees an empty range. `base` is 0 only for an unsynced state feeding
        // from genesis, which the syncer's genesis bootstrap prevents;
        // saturate to fail safe regardless.
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
        }

        self.publish_remaining();
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
            // Drain first: growth may already be queued.
            self.drain_events();
            if !self.hashes.is_empty() {
                return true;
            }
            if self.finished {
                return false;
            }

            match self.events.recv().await {
                Some(event) => {
                    self.apply(event);
                    self.drain_events();
                    // A Finish (or an empty extend) may still leave the range
                    // empty; report growth only if there is something to fetch.
                    !self.hashes.is_empty()
                }
                None => {
                    self.finished = true;
                    false
                }
            }
        })
    }

    fn extend(&mut self, hashes: &[block::Hash]) {
        self.hashes.extend(hashes.iter().copied());
        self.publish_remaining();
    }

    fn invalidate_above(&mut self, height: block::Height) {
        let keep = height.0.saturating_add(1).saturating_sub(self.base.0) as usize;
        self.hashes.truncate(keep);
        self.publish_remaining();
    }
}

#[cfg(test)]
mod tests;
