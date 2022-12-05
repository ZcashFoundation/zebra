//! Inventory Registry Implementation
//!
//! [RFC]: https://zebra.zfnd.org/dev/rfcs/0003-inventory-tracking.html

use std::{
    collections::HashMap,
    convert::TryInto,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{FutureExt, Stream, StreamExt};
use tokio::{
    sync::broadcast,
    time::{self, Instant},
};
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream, IntervalStream};

use zebra_chain::serialization::AtLeastOne;

use crate::{
    constants::INVENTORY_ROTATION_INTERVAL,
    protocol::{external::InventoryHash, internal::InventoryResponse},
    BoxError,
};

use self::update::Update;

/// Underlying type for the alias InventoryStatus::*
use InventoryResponse::*;

pub mod update;

#[cfg(test)]
mod tests;

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
pub type InventoryChange = InventoryStatus<(AtLeastOne<InventoryHash>, SocketAddr)>;

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
    current: HashMap<InventoryHash, HashMap<SocketAddr, InventoryMarker>>,

    /// Map tracking inventory statuses from the previous interval period.
    prev: HashMap<InventoryHash, HashMap<SocketAddr, InventoryMarker>>,

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
    pub fn new_available(hash: InventoryHash, peer: SocketAddr) -> Self {
        InventoryStatus::Available((AtLeastOne::from_one(hash), peer))
    }

    /// Returns a new missing inventory change from a single hash.
    #[allow(dead_code)]
    pub fn new_missing(hash: InventoryHash, peer: SocketAddr) -> Self {
        InventoryStatus::Missing((AtLeastOne::from_one(hash), peer))
    }

    /// Returns a new available multiple inventory change, if `hashes` contains at least one change.
    pub fn new_available_multi<'a>(
        hashes: impl IntoIterator<Item = &'a InventoryHash>,
        peer: SocketAddr,
    ) -> Option<Self> {
        let hashes: Vec<InventoryHash> = hashes.into_iter().copied().collect();
        let hashes = hashes.try_into().ok();

        hashes.map(|hashes| InventoryStatus::Available((hashes, peer)))
    }

    /// Returns a new missing multiple inventory change, if `hashes` contains at least one change.
    pub fn new_missing_multi<'a>(
        hashes: impl IntoIterator<Item = &'a InventoryHash>,
        peer: SocketAddr,
    ) -> Option<Self> {
        let hashes: Vec<InventoryHash> = hashes.into_iter().copied().collect();
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
        // SECURITY: if the rotation time is late, delay future rotations by the same amount
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

        Self {
            current: Default::default(),
            prev: Default::default(),
            inv_stream: BroadcastStream::new(inv_stream).boxed(),
            interval: IntervalStream::new(interval),
        }
    }

    /// Returns an iterator over addrs of peers that have recently advertised `hash` in their inventory.
    pub fn advertising_peers(&self, hash: InventoryHash) -> impl Iterator<Item = &SocketAddr> {
        self.status_peers(hash)
            .filter_map(|addr_status| addr_status.available())
    }

    /// Returns an iterator over addrs of peers that have recently missed `hash` in their inventory.
    #[allow(dead_code)]
    pub fn missing_peers(&self, hash: InventoryHash) -> impl Iterator<Item = &SocketAddr> {
        self.status_peers(hash)
            .filter_map(|addr_status| addr_status.missing())
    }

    /// Returns an iterator over peer inventory statuses for `hash`.
    ///
    /// Prefers current statuses to previously rotated statuses for the same peer.
    pub fn status_peers(
        &self,
        hash: InventoryHash,
    ) -> impl Iterator<Item = InventoryStatus<&SocketAddr>> {
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
    pub fn has_current_status(&self, hash: InventoryHash, addr: SocketAddr) -> bool {
        self.current
            .get(&hash)
            .and_then(|current| current.get(&addr))
            .is_some()
    }

    /// Returns a future that polls once for new registry updates.
    #[allow(dead_code)]
    pub fn update(&mut self) -> Update {
        Update::new(self)
    }

    /// Drive periodic inventory tasks
    ///
    /// # Details
    ///
    /// - rotates HashMaps based on interval events
    /// - drains the inv_stream channel and registers all advertised inventory
    pub fn poll_inventory(&mut self, cx: &mut Context<'_>) -> Result<(), BoxError> {
        // Correctness: Registers the current task for wakeup when the timer next becomes ready.
        while Pin::new(&mut self.interval).poll_next(cx).is_ready() {
            self.rotate();
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
        while let Poll::Ready(channel_result) = self.inv_stream.next().poll_unpin(cx) {
            match channel_result {
                Some(Ok(change)) => self.register(change),
                Some(Err(BroadcastStreamRecvError::Lagged(count))) => {
                    metrics::counter!("pool.inventory.dropped", 1);
                    metrics::counter!("pool.inventory.dropped.messages", count);

                    // If this message happens a lot, we should improve inventory registry performance,
                    // or poll the registry or peer set in a separate task.
                    info!(count, "dropped lagged inventory advertisements");
                }
                // This indicates all senders, including the one in the handshaker,
                // have been dropped, which really is a permanent failure.
                None => return Err(broadcast::error::RecvError::Closed.into()),
            }
        }

        Ok(())
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

            let current = self.current.entry(inv).or_default();

            // # Security
            //
            // Prefer `missing` over `advertised`, so malicious peers can't reset their own entries,
            // and funnel multiple failing requests to themselves.
            if let Some(old_status) = current.get(&addr) {
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

            let replaced_status = current.insert(addr, new_status);
            debug!(
                ?new_status,
                ?replaced_status,
                ?addr,
                ?inv,
                "inserted new status"
            );
        }
    }

    /// Replace the prev HashMap with current's and replace current with an empty
    /// HashMap
    fn rotate(&mut self) {
        self.prev = std::mem::take(&mut self.current);
    }
}
