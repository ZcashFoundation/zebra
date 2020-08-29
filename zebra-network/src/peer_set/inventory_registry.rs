use crate::protocol::external::InventoryHash;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
};

#[derive(Debug, Default)]
pub(super) struct InventoryRegistry {
    current: HashMap<InventoryHash, HashSet<SocketAddr>>,
    prev: HashMap<InventoryHash, HashSet<SocketAddr>>,
}

impl InventoryRegistry {
    pub fn register(&mut self, item: InventoryHash, addr: SocketAddr) {
        self.current.entry(item).or_default().insert(addr);
    }

    pub fn rotate(&mut self) {
        self.prev = std::mem::take(&mut self.current);
    }

    pub fn peers(&self, item: &InventoryHash) -> impl Iterator<Item = &SocketAddr> {
        let prev = self
            .prev
            .get(item)
            .into_iter()
            .flat_map(|addrs| addrs.iter());

        let current = self
            .current
            .get(item)
            .into_iter()
            .flat_map(|addrs| addrs.iter());

        prev.chain(current)
    }
}
