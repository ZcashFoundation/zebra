//! Inventory Registry Implementation
//!
//! [RFC]: https://zebra.zfnd.org/dev/rfcs/0003-inventory-tracking.html
use crate::{protocol::external::InventoryHash, BoxedStdError};
use futures::Stream;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    sync::broadcast,
    time::{self, Interval},
};

/// An Inventory Registry for tracking recent inventory advertisements by peer.
///
/// For more details please refer to the [RFC].
///
/// [RFC]: https://zebra.zfnd.org/dev/rfcs/0003-inventory-tracking.html
#[derive(Debug)]
pub struct InventoryRegistry {
    /// Map tracking the inventory advertisements from the current interval
    /// period
    current: HashMap<InventoryHash, HashSet<SocketAddr>>,
    /// Map tracking inventory advertisements from the previous interval period
    prev: HashMap<InventoryHash, HashSet<SocketAddr>>,
    /// Stream of incoming inventory hashes to register
    inv_stream: broadcast::Receiver<(InventoryHash, SocketAddr)>,
    /// Interval tracking how frequently we should rotate our maps
    interval: Interval,
}

impl InventoryRegistry {
    /// Returns an Inventory Registry
    pub fn new(inv_stream: broadcast::Receiver<(InventoryHash, SocketAddr)>) -> Self {
        Self {
            current: Default::default(),
            prev: Default::default(),
            inv_stream,
            interval: time::interval(Duration::from_secs(75)),
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
    pub fn poll_inventory(&mut self, cx: &mut Context<'_>) -> Result<(), BoxedStdError> {
        while let Poll::Ready(_) = self.interval.poll_tick(cx) {
            self.rotate();
        }

        while let Poll::Ready(Some((hash, addr))) = Pin::new(&mut self.inv_stream).poll_next(cx)? {
            self.register(hash, addr)
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
