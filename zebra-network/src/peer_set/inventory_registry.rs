//! Inventory Registry Implementation
//!
//! [RFC]: https://zebra.zfnd.org/dev/rfcs/0003-inventory-tracking.html

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{FutureExt, Stream, StreamExt};
use indexmap::IndexMap;
use tokio::{
    sync::broadcast,
    time::{self, Instant},
};
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream, IntervalStream};

use zebra_chain::serialization::AtLeastOne;

use crate::{
    constants::INVENTORY_ROTATION_INTERVAL,
    protocol::{external::InventoryHash, internal::InventoryResponse},
    BoxError, PeerSocketAddr,
};

use self::update::Update;

/// Underlying type for the alias InventoryStatus::*
use InventoryResponse::*;

pub mod update;

#[cfg(test)]
mod tests;

/// The maximum number of inventory hashes we will track from a single peer.
///
/// # Security
///
/// This limits known memory denial of service attacks like <https://invdos.net/> to a total of:
/// ```text
/// 1000 inventory * 2 maps * 32-64 bytes per inventory = less than 1 MB
/// 1000 inventory * 70 peers * 2 maps * 6-18 bytes per address = up to 3 MB
/// ```
///
/// Since the inventory registry is an efficiency optimisation, which falls back to a
/// random peer, we only need to track a small number of hashes for available inventory.
///
/// But we want to be able to track a significant amount of missing inventory,
/// to limit queries for globally missing inventory.
//
// TODO: split this into available (25) and missing (1000 or more?)
pub const MAX_INV_PER_MAP: usize = 1000;

/// The maximum number of peers we will track inventory for.
///
/// # Security
///
/// This limits known memory denial of service attacks. See [`MAX_INV_PER_MAP`] for details.
///
/// Since the inventory registry is an efficiency optimisation, which falls back to a
/// random peer, we only need to track a small number of peers per inv for available inventory.
///
/// But we want to be able to track missing inventory for almost all our peers,
/// so we only query a few peers for inventory that is genuinely missing from the network.
//
// TODO: split this into available (25) and missing (70)
pub const MAX_PEERS_PER_INV: usize = 70;

/// A peer inventory status, which tracks a hash for both available and missing inventory.
pub type InventoryStatus<T> = InventoryResponse<T, T>;

/// A peer inventory status change, used in the inventory status channel.
///
/// For performance reasons, advertisements should only be tracked
/// for hashes that are rare on the network.
/// So Zebra only tracks single-block inventory messages.
///
/// For security reasons, all `notfound` rejections should be tracked.
/// This also helps with performance, if the hash is rare on the network.
pub type InventoryChange = InventoryStatus<(AtLeastOne<InventoryHash>, PeerSocketAddr)>;

/// An internal marker used in inventory status hash maps.
type InventoryMarker = InventoryStatus<()>;

/// An Inventory Registry for tracking recent inventory advertisements and missing inventory.
///
/// For more details please refer to the [RFC].
///
/// [RFC]: https://zebra.zfnd.org/dev/rfcs/0003-inventory-tracking.html
pub struct InventoryRegistry {
    /// Map tracking the latest inventory status from the current interval
    /// period.
    //
    // TODO: split maps into available and missing, so we can limit them separately.
    current: IndexMap<InventoryHash, IndexMap<PeerSocketAddr, InventoryMarker>>,

    /// Map tracking inventory statuses from the previous interval period.
    prev: IndexMap<InventoryHash, IndexMap<PeerSocketAddr, InventoryMarker>>,

    /// Stream of incoming inventory statuses to register.
    inv_stream: Pin<
        Box<dyn Stream<Item = Result<InventoryChange, BroadcastStreamRecvError>> + Send + 'static>,
    >,

    /// Interval tracking when we should next rotate our maps.
    interval: IntervalStream,
}

impl std::fmt::Debug for InventoryRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InventoryRegistry")
            .field("current", &self.current)
            .field("prev", &self.prev)
            .finish()
    }
}

impl InventoryChange {
    /// Returns a new available inventory change from a single hash.
    pub fn new_available(hash: InventoryHash, peer: PeerSocketAddr) -> Self {
        InventoryStatus::Available((AtLeastOne::from_one(hash), peer))
    }

    /// Returns a new missing inventory change from a single hash.
    #[allow(dead_code)]
    pub fn new_missing(hash: InventoryHash, peer: PeerSocketAddr) -> Self {
        InventoryStatus::Missing((AtLeastOne::from_one(hash), peer))
    }

    /// Returns a new available multiple inventory change, if `hashes` contains at least one change.
    pub fn new_available_multi<'a>(
        hashes: impl IntoIterator<Item = &'a InventoryHash>,
        peer: PeerSocketAddr,
    ) -> Option<Self> {
        let mut hashes: Vec<InventoryHash> = hashes.into_iter().copied().collect();

        // # Security
        //
        // Don't send more hashes than we're going to store.
        // It doesn't matter which hashes we choose, because this is an efficiency optimisation.
        //
        //  This limits known memory denial of service attacks to:
        // `1000 hashes * 200 peers/channel capacity * 32-64 bytes = up to 12 MB`
        hashes.truncate(MAX_INV_PER_MAP);

        let hashes = hashes.try_into().ok();

        hashes.map(|hashes| InventoryStatus::Available((hashes, peer)))
    }

    /// Returns a new missing multiple inventory change, if `hashes` contains at least one change.
    pub fn new_missing_multi<'a>(
        hashes: impl IntoIterator<Item = &'a InventoryHash>,
        peer: PeerSocketAddr,
    ) -> Option<Self> {
        let mut hashes: Vec<InventoryHash> = hashes.into_iter().copied().collect();

        // # Security
        //
        // Don't send more hashes than we're going to store.
        // It doesn't matter which hashes we choose, because this is an efficiency optimisation.
        hashes.truncate(MAX_INV_PER_MAP);

        let hashes = hashes.try_into().ok();

        hashes.map(|hashes| InventoryStatus::Missing((hashes, peer)))
    }
}

impl<T> InventoryStatus<T> {
    /// Get a marker for the status, without any associated data.
    pub fn marker(&self) -> InventoryMarker {
        self.as_ref().map(|_inner| ())
    }

    /// Maps an `InventoryStatus<T>` to `InventoryStatus<U>` by applying a function to a contained value.
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> InventoryStatus<U> {
        // Based on Option::map from https://doc.rust-lang.org/src/core/option.rs.html#844
        match self {
            Available(item) => Available(f(item)),
            Missing(item) => Missing(f(item)),
        }
    }
}

impl<T: Clone> InventoryStatus<T> {
    /// Returns a clone of the inner item, regardless of status.
    pub fn to_inner(&self) -> T {
        match self {
            Available(item) | Missing(item) => item.clone(),
        }
    }
}

impl InventoryRegistry {
    /// Returns a new Inventory Registry for `inv_stream`.
    pub fn new(inv_stream: broadcast::Receiver<InventoryChange>) -> Self {
        let interval = INVENTORY_ROTATION_INTERVAL;

        // Don't do an immediate rotation, current and prev are already empty.
        let mut interval = tokio::time::interval_at(Instant::now() + interval, interval);
        // # Security
        //
        // If the rotation time is late, execute as many ticks as needed to catch up.
        // This is a tradeoff between memory usage and quickly accessing remote data
        // under heavy load. Bursting prioritises lower memory usage.
        //
        // Skipping or delaying could keep peer inventory in memory for a longer time,
        // further increasing memory load or delays due to virtual memory swapping.
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Burst);

        Self {
            current: Default::default(),
            prev: Default::default(),
            inv_stream: BroadcastStream::new(inv_stream).boxed(),
            interval: IntervalStream::new(interval),
        }
    }

    /// Returns an iterator over addrs of peers that have recently advertised `hash` in their inventory.
    pub fn advertising_peers(&self, hash: InventoryHash) -> impl Iterator<Item = &PeerSocketAddr> {
        self.status_peers(hash)
            .filter_map(|addr_status| addr_status.available())
    }

    /// Returns an iterator over addrs of peers that have recently missed `hash` in their inventory.
    #[allow(dead_code)]
    pub fn missing_peers(&self, hash: InventoryHash) -> impl Iterator<Item = &PeerSocketAddr> {
        self.status_peers(hash)
            .filter_map(|addr_status| addr_status.missing())
    }

    /// Returns an iterator over peer inventory statuses for `hash`.
    ///
    /// Prefers current statuses to previously rotated statuses for the same peer.
    pub fn status_peers(
        &self,
        hash: InventoryHash,
    ) -> impl Iterator<Item = InventoryStatus<&PeerSocketAddr>> {
        let prev = self.prev.get(&hash);
        let current = self.current.get(&hash);

        // # Security
        //
        // Prefer `current` statuses for the same peer over previously rotated statuses.
        // This makes sure Zebra is using the most up-to-date network information.
        let prev = prev
            .into_iter()
            .flatten()
            .filter(move |(addr, _status)| !self.has_current_status(hash, **addr));
        let current = current.into_iter().flatten();

        current
            .chain(prev)
            .map(|(addr, status)| status.map(|()| addr))
    }

    /// Returns true if there is a current status entry for `hash` and `addr`.
    pub fn has_current_status(&self, hash: InventoryHash, addr: PeerSocketAddr) -> bool {
        self.current
            .get(&hash)
            .and_then(|current| current.get(&addr))
            .is_some()
    }

    /// Returns an iterator over peer inventory status hashes.
    ///
    /// Yields current statuses first, then previously rotated statuses.
    /// This can include multiple statuses for the same hash.
    #[allow(dead_code)]
    pub fn status_hashes(
        &self,
    ) -> impl Iterator<Item = (&InventoryHash, &IndexMap<PeerSocketAddr, InventoryMarker>)> {
        self.current.iter().chain(self.prev.iter())
    }

    /// Returns a future that waits for new registry updates.
    #[allow(dead_code)]
    pub fn update(&mut self) -> Update<'_> {
        Update::new(self)
    }

    /// Drive periodic inventory tasks.
    ///
    /// Rotates the inventory HashMaps on every timer tick.
    /// Drains the inv_stream channel and registers all advertised inventory.
    ///
    /// Returns an error if the inventory channel is closed.
    ///
    /// Otherwise, returns `Ok` if it performed at least one update or rotation, or `Poll::Pending`
    /// if there was no inventory change. Always registers a wakeup for the next inventory update
    /// or rotation, even when it returns `Ok`.
    pub fn poll_inventory(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        let mut result = Poll::Pending;

        // # Correctness
        //
        // Registers the current task for wakeup when the timer next becomes ready.
        // (But doesn't return, because we also want to register the task for wakeup when more
        // inventory arrives.)
        //
        // # Security
        //
        // Only rotate one inventory per peer request, to give the next inventory
        // time to gather some peer advertisements. This is a tradeoff between
        // memory usage and quickly accessing remote data under heavy load.
        //
        // This prevents a burst edge case where all inventory is emptied after
        // two interval ticks are delayed.
        if Pin::new(&mut self.interval).poll_next(cx).is_ready() {
            self.rotate();
            result = Poll::Ready(Ok(()));
        }

        // This module uses a broadcast channel instead of an mpsc channel, even
        // though there's a single consumer of inventory advertisements, because
        // the broadcast channel has ring-buffer behavior: when the channel is
        // full, sending a new message displaces the oldest message in the
        // channel.
        //
        // This is the behavior we want for inventory advertisements, because we
        // want to have a bounded buffer of unprocessed advertisements, and we
        // want to prioritize new inventory (which is likely only at a specific
        // peer) over old inventory (which is likely more widely distributed).
        //
        // The broadcast channel reports dropped messages by returning
        // `RecvError::Lagged`. It's crucial that we handle that error here
        // rather than propagating it through the peer set's Service::poll_ready
        // implementation, where reporting a failure means reporting a permanent
        // failure of the peer set.

        // Returns Pending if all messages are processed, but the channel could get more.
        loop {
            let channel_result = self.inv_stream.next().poll_unpin(cx);

            match channel_result {
                Poll::Ready(Some(Ok(change))) => {
                    self.register(change);
                    result = Poll::Ready(Ok(()));
                }
                Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(count)))) => {
                    // This isn't a fatal inventory error, it's expected behaviour when Zebra is
                    // under load from peers.
                    metrics::counter!("pool.inventory.dropped").increment(1);
                    metrics::counter!("pool.inventory.dropped.messages").increment(count);

                    // If this message happens a lot, we should improve inventory registry
                    // performance, or poll the registry or peer set in a separate task.
                    info!(count, "dropped lagged inventory advertisements");
                }
                Poll::Ready(None) => {
                    // If the channel is empty and returns None, all senders, including the one in
                    // the handshaker, have been dropped, which really is a permanent failure.
                    result = Poll::Ready(Err(broadcast::error::RecvError::Closed.into()));
                }
                Poll::Pending => {
                    break;
                }
            }
        }

        result
    }

    /// Record the given inventory `change` for the peer `addr`.
    ///
    /// `Missing` markers are not updated until the registry rotates, for security reasons.
    fn register(&mut self, change: InventoryChange) {
        let new_status = change.marker();
        let (invs, addr) = change.to_inner();

        for inv in invs {
            use InventoryHash::*;
            assert!(
                matches!(inv, Block(_) | Tx(_) | Wtx(_)),
                "unexpected inventory type: {inv:?} from peer: {addr:?}",
            );

            let hash_peers = self.current.entry(inv).or_default();

            // # Security
            //
            // Prefer `missing` over `advertised`, so malicious peers can't reset their own entries,
            // and funnel multiple failing requests to themselves.
            if let Some(old_status) = hash_peers.get(&addr) {
                if old_status.is_missing() && new_status.is_available() {
                    debug!(?new_status, ?old_status, ?addr, ?inv, "skipping new status");
                    continue;
                }

                debug!(
                    ?new_status,
                    ?old_status,
                    ?addr,
                    ?inv,
                    "keeping both new and old status"
                );
            }

            let replaced_status = hash_peers.insert(addr, new_status);

            debug!(
                ?new_status,
                ?replaced_status,
                ?addr,
                ?inv,
                "inserted new status"
            );

            // # Security
            //
            // Limit the number of stored peers per hash, removing the oldest entries,
            // because newer entries are likely to be more relevant.
            //
            // TODO: do random or weighted random eviction instead?
            if hash_peers.len() > MAX_PEERS_PER_INV {
                // Performance: `MAX_PEERS_PER_INV` is small, so O(n) performance is acceptable.
                hash_peers.shift_remove_index(0);
            }

            // # Security
            //
            // Limit the number of stored inventory hashes, removing the oldest entries,
            // because newer entries are likely to be more relevant.
            //
            // TODO: do random or weighted random eviction instead?
            if self.current.len() > MAX_INV_PER_MAP {
                // Performance: `MAX_INV_PER_MAP` is small, so O(n) performance is acceptable.
                self.current.shift_remove_index(0);
            }
        }
    }

    /// Replace the prev HashMap with current's and replace current with an empty
    /// HashMap
    fn rotate(&mut self) {
        self.prev = std::mem::take(&mut self.current);
    }
}
