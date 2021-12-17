//! Inventory Registry Implementation
//!
//! [RFC]: https://zebra.zfnd.org/dev/rfcs/0003-inventory-tracking.html

use std::{
    collections::HashMap,
    convert::TryInto,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use futures::{FutureExt, Stream, StreamExt};
use tokio::{sync::broadcast, time};
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream, IntervalStream};

use zebra_chain::{parameters::POST_BLOSSOM_POW_TARGET_SPACING, serialization::AtLeastOne};

use crate::{protocol::external::InventoryHash, BoxError};

use InventoryStatus::*;

/// A peer inventory status change, used in the inventory status channel.
pub type InventoryChange = InventoryStatus<(AtLeastOne<InventoryHash>, SocketAddr)>;

/// An internal marker used in inventory status hash maps.
type InventoryMarker = InventoryStatus<()>;

/// A generic peer inventory status.
///
/// `Advertised` is used for inventory that peers claim to have,
/// and `Missing` is used for inventory they didn't provide when we requested it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum InventoryStatus<T: Clone> {
    /// An advertised inventory hash.
    ///
    /// For performance reasons, advertisements should only be sent for hashes that are rare on the network.
    Advertised(T),

    /// An inventory hash rejected by a peer.
    ///
    /// For security reasons, all `notfound` rejections should be tracked.
    /// This also helps with performance, if the hash is rare.
    #[allow(dead_code)]
    Missing(T),
}

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

impl<T: Clone> InventoryStatus<T> {
    /// Returns true if the inventory item was advertised.
    #[allow(dead_code)]
    pub fn is_advertised(&self) -> bool {
        matches!(self, Advertised(_))
    }

    /// Returns true if the inventory item was missing.
    #[allow(dead_code)]
    pub fn is_missing(&self) -> bool {
        matches!(self, Missing(_))
    }

    /// Get the advertised inventory item, if present.
    pub fn advertised(&self) -> Option<T> {
        if let Advertised(item) = self {
            Some(item.clone())
        } else {
            None
        }
    }

    /// Get the rejected inventory item, if present.
    #[allow(dead_code)]
    pub fn missing(&self) -> Option<T> {
        if let Missing(item) = self {
            Some(item.clone())
        } else {
            None
        }
    }

    /// Get the inner item, regardless of status.
    pub fn inner(&self) -> T {
        match self {
            Advertised(item) | Missing(item) => item.clone(),
        }
    }

    /// Maps an `InventoryStatus<T>` to `InventoryStatus<U>` by applying a function to a contained value.
    pub fn map<U: Clone, F: FnOnce(T) -> U>(self, f: F) -> InventoryStatus<U> {
        // Based on Option::map from https://doc.rust-lang.org/src/core/option.rs.html#829
        match self {
            Advertised(item) => Advertised(f(item)),
            Missing(item) => Missing(f(item)),
        }
    }

    /// Converts from `&InventoryStatus<T>` to `InventoryStatus<&T>`.
    pub fn as_ref(&self) -> InventoryStatus<&T> {
        match self {
            Advertised(item) => Advertised(item),
            Missing(item) => Missing(item),
        }
    }
}

impl InventoryRegistry {
    /// Returns a new Inventory Registry for `inv_stream`.
    pub fn new(inv_stream: broadcast::Receiver<InventoryChange>) -> Self {
        Self {
            current: Default::default(),
            prev: Default::default(),
            inv_stream: BroadcastStream::new(inv_stream).boxed(),
            interval: IntervalStream::new(time::interval(Duration::from_secs(
                POST_BLOSSOM_POW_TARGET_SPACING
                    .try_into()
                    .expect("non-negative"),
            ))),
        }
    }

    /// Returns an iterator over addrs of peers that have recently advertised
    /// having `hash` in their inventory.
    pub fn advertising_peers(&self, hash: &InventoryHash) -> impl Iterator<Item = &SocketAddr> {
        let prev = self.prev.get(hash).into_iter();
        let current = self.current.get(hash).into_iter();

        prev.chain(current)
            .flatten()
            .filter_map(|(addr, status)| status.advertised().map(|()| addr))
    }

    /// Returns an iterator over addrs of peers that are recently missing `hash` in their inventory.
    #[allow(dead_code)]
    pub fn missing_peers(&self, hash: &InventoryHash) -> impl Iterator<Item = &SocketAddr> {
        let prev = self.prev.get(hash).into_iter();
        let current = self.current.get(hash).into_iter();

        prev.chain(current)
            .flatten()
            .filter_map(|(addr, status)| status.missing().map(|()| addr))
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
                    tracing::debug!(count, "dropped lagged inventory advertisements");
                }
                // This indicates all senders, including the one in the handshaker,
                // have been dropped, which really is a permanent failure.
                None => return Err(broadcast::error::RecvError::Closed.into()),
            }
        }

        Ok(())
    }

    /// Record the given inventory `change` for the peer `addr`.
    fn register(&mut self, change: InventoryChange) {
        let status = change.as_ref().map(|_| ());
        let (invs, addr) = change.inner();

        for inv in invs {
            self.current.entry(inv).or_default().insert(addr, status);
        }
    }

    /// Replace the prev HashMap with current's and replace current with an empty
    /// HashMap
    fn rotate(&mut self) {
        self.prev = std::mem::take(&mut self.current);
    }
}
