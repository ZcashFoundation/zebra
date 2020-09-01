use crate::{protocol::external::InventoryHash, BoxedStdError};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    task::{Context, Poll},
};
use tokio::sync::broadcast;

#[derive(Debug)]
pub(super) struct InventoryRegistry {
    current: HashMap<InventoryHash, HashSet<SocketAddr>>,
    prev: HashMap<InventoryHash, HashSet<SocketAddr>>,
    /// Stream of incoming inventory hashes to
    inv_stream: broadcast::Receiver<(InventoryHash, SocketAddr)>,
}

impl InventoryRegistry {
    pub fn new(inv_stream: broadcast::Receiver<(InventoryHash, SocketAddr)>) -> Self {
        Self {
            current: Default::default(),
            prev: Default::default(),
            inv_stream,
        }
    }

    pub fn peers(&self, item: &InventoryHash) -> impl Iterator<Item = &SocketAddr> {
        let prev = self.prev.get(item).into_iter();
        let current = self.current.get(item).into_iter();

        prev.chain(current).flatten()
    }

    pub fn poll_inventory(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxedStdError>> {
        todo!()
    }

    fn register(&mut self, item: InventoryHash, addr: SocketAddr) {
        self.current.entry(item).or_default().insert(addr);
    }

    fn rotate(&mut self) {
        self.prev = std::mem::take(&mut self.current);
    }
}
