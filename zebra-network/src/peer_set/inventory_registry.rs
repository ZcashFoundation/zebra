//! Inventory Registry Implementation
//!
//! [RFC]: https://zebra.zfnd.org/dev/rfcs/0003-inventory-tracking.html

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use futures::{FutureExt, Stream, StreamExt};
use tokio::{sync::broadcast, time};
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream, IntervalStream};

use crate::{protocol::external::InventoryHash, BoxError};

/// An Inventory Registry for tracking recent inventory advertisements by peer.
///
/// For more details please refer to the [RFC].
///
/// [RFC]: https://zebra.zfnd.org/dev/rfcs/0003-inventory-tracking.html
pub struct InventoryRegistry {
    /// Map tracking the inventory advertisements from the current interval
    /// period
    current: HashMap<InventoryHash, HashSet<SocketAddr>>,
    /// Map tracking inventory advertisements from the previous interval period
    prev: HashMap<InventoryHash, HashSet<SocketAddr>>,
    /// Stream of incoming inventory hashes to register
    inv_stream: Pin<
        Box<
            dyn Stream<Item = Result<(InventoryHash, SocketAddr), BroadcastStreamRecvError>>
                + Send
                + 'static,
        >,
    >,
    /// Interval tracking how frequently we should rotate our maps
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

impl InventoryRegistry {
    /// Returns an Inventory Registry
    pub fn new(inv_stream: broadcast::Receiver<(InventoryHash, SocketAddr)>) -> Self {
        Self {
            current: Default::default(),
            prev: Default::default(),
            inv_stream: BroadcastStream::new(inv_stream).boxed(),
            interval: IntervalStream::new(time::interval(Duration::from_secs(75))),
        }
    }

    /// Returns an iterator over addrs of peers that have recently advertised
    /// having `hash` in their inventory.
    pub fn peers(&self, hash: &InventoryHash) -> impl Iterator<Item = &SocketAddr> {
        let prev = self.prev.get(hash).into_iter();
        let current = self.current.get(hash).into_iter();

        prev.chain(current).flatten()
    }

    /// Drive periodic inventory tasks
    ///
    /// # Details
    ///
    /// - rotates HashMaps based on interval events
    /// - drains the inv_stream channel and registers all advertised inventory
    pub fn poll_inventory(&mut self, cx: &mut Context<'_>) -> Result<(), BoxError> {
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
                Some(Ok((hash, addr))) => self.register(hash, addr),
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

    /// Record that the given inventory `hash` is available from the peer `addr`
    fn register(&mut self, hash: InventoryHash, addr: SocketAddr) {
        self.current.entry(hash).or_default().insert(addr);
    }

    /// Replace the prev HashMap with current's and replace current with an empty
    /// HashMap
    fn rotate(&mut self) {
        self.prev = std::mem::take(&mut self.current);
    }
}
