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

#[derive(Debug)]
pub struct InventoryRegistry {
    current: HashMap<InventoryHash, HashSet<SocketAddr>>,
    prev: HashMap<InventoryHash, HashSet<SocketAddr>>,
    /// Stream of incoming inventory hashes to
    inv_stream: broadcast::Receiver<(InventoryHash, SocketAddr)>,
    interval: Interval,
}

impl InventoryRegistry {
    pub fn new(inv_stream: broadcast::Receiver<(InventoryHash, SocketAddr)>) -> Self {
        Self {
            current: Default::default(),
            prev: Default::default(),
            inv_stream,
            interval: time::interval(Duration::from_secs(75)),
        }
    }

    pub fn peers(&self, hash: &InventoryHash) -> impl Iterator<Item = &SocketAddr> {
        let prev = self.prev.get(hash).into_iter();
        let current = self.current.get(hash).into_iter();

        prev.chain(current).flatten()
    }

    pub fn poll_inventory(&mut self, cx: &mut Context<'_>) -> Result<(), BoxedStdError> {
        while let Poll::Ready(_) = self.interval.poll_tick(cx) {
            self.rotate();
        }

        while let Poll::Ready(Some((hash, addr))) = Pin::new(&mut self.inv_stream).poll_next(cx)? {
            self.register(hash, addr)
        }

        Ok(())
    }

    fn register(&mut self, hash: InventoryHash, addr: SocketAddr) {
        self.current.entry(hash).or_default().insert(addr);
    }

    fn rotate(&mut self) {
        self.prev = std::mem::take(&mut self.current);
    }
}
