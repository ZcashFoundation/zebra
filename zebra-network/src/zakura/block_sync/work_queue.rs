//! Download work source for Zakura block sync.
//!
//! The [`WorkQueue`] is the sole shared download-scheduling primitive: a sorted
//! set of needed block heights the per-peer issuance path pulls from. It replaces
//! the central `BlockRangeScheduler`'s eligibility/dedup/retry roles with a small
//! API the caller drives from its own per-peer state (see the design doc §2/§3):
//!
//! - a height is in **exactly one** of `{below-floor (gone), pending, in_flight}`;
//! - [`take_in_range`](WorkQueue::take_in_range) moves a contiguous-ascending run
//!   `pending → in_flight` (so one taken chunk maps to one `BlockRangeRequest`),
//!   bounded only by the caller's servable range and a count cap — never by how
//!   far above the committed floor the heights already are;
//! - only [`return_items`](WorkQueue::return_items) (timeout/disconnect retry) and
//!   [`reset_above`](WorkQueue::reset_above) move `in_flight → pending`;
//! - [`advance_floor`](WorkQueue::advance_floor) is garbage collection only — the
//!   committed floor never throttles the fetch decision.
//!
//! Internals are a brief `std::sync::Mutex` whose critical sections are tiny map
//! splices held **never across `.await`** (the anti-block rule). `estimated_bytes`
//! on a [`WorkItem`] is the block's size *estimate* (not its worst-case
//! reservation); it exists only to carry the `SizeMismatch` tolerance check
//! through to the reactor's receive path.

use std::sync::Mutex as StdMutex;

use tokio::sync::Notify;
use zebra_chain::block;

use super::request::BlockSizeEstimate;

/// EWMA seed used as the fallback body-size estimate when a height has no size
/// hint. Moved here from the old scheduler; the estimate only feeds the
/// `SizeMismatch` tolerance check, never the byte reservation.
pub(super) const DEFAULT_BS_EWMA_SEED_BYTES: u64 = 256 * 1024;
/// Lower clamp on a body-size estimate.
pub(super) const DEFAULT_BS_SIZE_FLOOR_BYTES: u64 = 1024;

/// Per-height download metadata held in the [`WorkQueue`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct WorkItem {
    /// Expected hash of the block at this height (drives the response match).
    pub(super) hash: block::Hash,
    /// The block's *size estimate* (not its worst-case byte reservation). Only
    /// used to feed the receive-path `SizeMismatch` tolerance check.
    pub(super) estimated_bytes: u64,
}

#[derive(Debug)]
struct WorkQueueInner {
    pending: std::collections::BTreeMap<block::Height, WorkItem>,
    in_flight: std::collections::BTreeMap<block::Height, WorkItem>,
    floor: block::Height,
    /// EWMA fallback estimate (overridable for tests).
    ewma_fallback_bytes: u64,
    /// Floor clamp for size estimates (overridable for tests).
    floor_estimate_bytes: u64,
}

impl WorkQueueInner {
    fn estimate_bytes(&self, estimate: BlockSizeEstimate) -> u64 {
        estimate_bytes_with(
            estimate,
            self.ewma_fallback_bytes,
            self.floor_estimate_bytes,
        )
    }
}

/// Compute a clamped body-size estimate from a [`BlockSizeEstimate`] hint.
///
/// `Confirmed`/`Advertised` use the hinted size; `Unknown` uses the EWMA seed.
/// The result is clamped to `[floor, MAX_BLOCK_BYTES]`. Preserved verbatim from
/// the old scheduler's `estimate_bytes`.
fn estimate_bytes_with(estimate: BlockSizeEstimate, ewma: u64, floor: u64) -> u64 {
    let hinted = match estimate {
        BlockSizeEstimate::Confirmed(size) | BlockSizeEstimate::Advertised(size) => u64::from(size),
        BlockSizeEstimate::Unknown => ewma,
    };
    hinted.max(floor).min(block::MAX_BLOCK_BYTES)
}

/// The shared download work source. See the module docs for the invariants.
#[derive(Debug)]
pub(super) struct WorkQueue {
    inner: StdMutex<WorkQueueInner>,
    available: Notify,
}

impl WorkQueue {
    pub(super) fn new(floor: block::Height) -> Self {
        Self {
            inner: StdMutex::new(WorkQueueInner {
                pending: std::collections::BTreeMap::new(),
                in_flight: std::collections::BTreeMap::new(),
                floor,
                ewma_fallback_bytes: DEFAULT_BS_EWMA_SEED_BYTES,
                floor_estimate_bytes: DEFAULT_BS_SIZE_FLOOR_BYTES,
            }),
            available: Notify::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn set_estimator_for_tests(&self, ewma: u64, floor: u64) {
        let mut inner = self.lock();
        inner.ewma_fallback_bytes = ewma.max(1);
        inner.floor_estimate_bytes = floor.max(1);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WorkQueueInner> {
        self.inner
            .lock()
            .expect("work queue mutex is never poisoned")
    }

    /// Add `(height, hash, size)` items to `pending`. Each is inserted iff its
    /// height is `> floor` and not already in `pending` or `in_flight`
    /// (idempotent — already-buffered/fetched heights are never re-queued).
    /// Returns the number of newly-inserted heights and wakes waiters if any.
    pub(super) fn extend(
        &self,
        items: impl IntoIterator<Item = (block::Height, block::Hash, BlockSizeEstimate)>,
    ) -> usize {
        let mut inserted = 0usize;
        {
            let mut inner = self.lock();
            for (height, hash, size) in items {
                if height <= inner.floor
                    || inner.pending.contains_key(&height)
                    || inner.in_flight.contains_key(&height)
                {
                    continue;
                }
                let estimated_bytes = inner.estimate_bytes(size);
                inner.pending.insert(
                    height,
                    WorkItem {
                        hash,
                        estimated_bytes,
                    },
                );
                inserted += 1;
            }
        }
        if inserted > 0 {
            self.available.notify_waiters();
        }
        inserted
    }

    /// Move up to `max` contiguous-ascending `pending` heights within
    /// `low..=high` from `pending` to `in_flight`, returned in ascending order.
    ///
    /// "Contiguous-ascending" stops at the first gap, so the returned chunk maps
    /// to a single `BlockRangeRequest`. `high` is the caller's `servable_high`
    /// and is **NOT** clamped to the floor (the committed floor is never an upper
    /// bound on the fetch). Returns empty if nothing is eligible.
    pub(super) fn take_in_range(
        &self,
        low: block::Height,
        high: block::Height,
        max: usize,
    ) -> Vec<(block::Height, WorkItem)> {
        if max == 0 || low > high {
            return Vec::new();
        }
        let mut inner = self.lock();
        let mut taken: Vec<(block::Height, WorkItem)> = Vec::new();
        let mut next_expected: Option<block::Height> = None;
        for (height, item) in inner.pending.range(low..=high) {
            if let Some(expected) = next_expected {
                if *height != expected {
                    break;
                }
            }
            taken.push((*height, *item));
            if taken.len() >= max {
                break;
            }
            // Stop the run at the end of the height space rather than overflowing.
            match height.0.checked_add(1) {
                Some(raw) => next_expected = Some(block::Height(raw)),
                None => break,
            }
        }
        for (height, item) in &taken {
            inner.pending.remove(height);
            inner.in_flight.insert(*height, *item);
        }
        taken
    }

    /// Move each given height `in_flight → pending`, preserving its stored
    /// [`WorkItem`]. Heights not currently `in_flight` are skipped (idempotent).
    /// Wakes waiters if anything moved.
    pub(super) fn return_items(&self, heights: impl IntoIterator<Item = block::Height>) {
        let mut moved = false;
        {
            let mut inner = self.lock();
            for height in heights {
                if let Some(item) = inner.in_flight.remove(&height) {
                    inner.pending.insert(height, item);
                    moved = true;
                }
            }
        }
        if moved {
            self.available.notify_waiters();
        }
    }

    /// Like [`return_items`](Self::return_items) but **does not** notify waiters.
    ///
    /// Used by a peer routine to put back a chunk it took but chose not to issue
    /// (e.g. the heights are in its own short retry-avoid window after it just
    /// failed them). Notifying here would re-wake the returning routine's own
    /// freshly-registered `available` future and busy-loop the want-work arm
    /// (a self-wake spin); other peers were already woken by the original failure
    /// `return_items`, so suppressing the notify only affects the caller.
    pub(super) fn return_items_quiet(&self, heights: impl IntoIterator<Item = block::Height>) {
        let mut inner = self.lock();
        for height in heights {
            if let Some(item) = inner.in_flight.remove(&height) {
                inner.pending.insert(height, item);
            }
        }
    }

    /// Garbage-collect committed heights: raise the floor to `max(self.floor,
    /// floor)` and drop every `pending`/`in_flight` entry `<= floor`. Does **not** throttle
    /// how far above the floor peers may fetch.
    pub(super) fn advance_floor(&self, floor: block::Height) {
        let mut inner = self.lock();
        inner.floor = inner.floor.max(floor);
        let floor = inner.floor;
        inner.pending.retain(|height, _| *height > floor);
        inner.in_flight.retain(|height, _| *height > floor);
    }

    /// Frontier reset: pin the floor and drop every `pending`/`in_flight` entry
    /// `> floor` (their buffers were dropped; the producer re-fills via the next
    /// query).
    pub(super) fn reset_above(&self, floor: block::Height) {
        let mut inner = self.lock();
        inner.floor = floor;
        inner.pending.retain(|height, _| *height <= floor);
        inner.in_flight.retain(|height, _| *height <= floor);
    }

    /// The "work added" notifier (S4 wake source).
    #[allow(dead_code)]
    pub(super) fn subscribe_available(&self) -> &Notify {
        &self.available
    }

    // ---- diagnostics (trace + late-response classification) ----

    pub(super) fn pending_len(&self) -> usize {
        self.lock().pending.len()
    }

    pub(super) fn in_flight_len(&self) -> usize {
        self.lock().in_flight.len()
    }

    /// Number of contiguous runs across `pending` (the old `queue_len` meaning:
    /// one queued range per maximal contiguous run of heights).
    pub(super) fn pending_run_count(&self) -> usize {
        let inner = self.lock();
        let mut runs = 0usize;
        let mut previous: Option<block::Height> = None;
        for height in inner.pending.keys() {
            let contiguous =
                previous.and_then(|previous| previous.0.checked_add(1)) == Some(height.0);
            if !contiguous {
                runs += 1;
            }
            previous = Some(*height);
        }
        runs
    }

    pub(super) fn min_pending(&self) -> Option<block::Height> {
        self.lock().pending.keys().next().copied()
    }

    pub(super) fn max_in_flight(&self) -> Option<block::Height> {
        self.lock().in_flight.keys().next_back().copied()
    }

    pub(super) fn max_claimed(&self) -> Option<block::Height> {
        let inner = self.lock();
        inner
            .pending
            .keys()
            .next_back()
            .copied()
            .max(inner.in_flight.keys().next_back().copied())
    }

    /// Expected hash for a height in `pending` or `in_flight` (late-response
    /// recovery; replaces the old `queued_hash_for_height`).
    pub(super) fn hash_for_height(&self, height: block::Height) -> Option<block::Hash> {
        let inner = self.lock();
        inner
            .pending
            .get(&height)
            .or_else(|| inner.in_flight.get(&height))
            .map(|item| item.hash)
    }

    pub(super) fn pending_contains(&self, height: block::Height) -> bool {
        self.lock().pending.contains_key(&height)
    }

    pub(super) fn in_flight_contains(&self, height: block::Height) -> bool {
        self.lock().in_flight.contains_key(&height)
    }
}
