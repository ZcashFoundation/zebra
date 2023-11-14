//! Inventory registry update future.

use std::pin::Pin;

use futures::{
    future::Future,
    task::{Context, Poll},
};

use crate::{peer_set::InventoryRegistry, BoxError};

/// Future for the [`update`](super::InventoryRegistry::update) method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Update<'a> {
    registry: &'a mut InventoryRegistry,
}

impl Unpin for Update<'_> {}

impl<'a> Update<'a> {
    /// Returns a new future that returns when the next inventory update or rotation has been
    /// completed by `registry`.
    ///
    /// See [`InventoryRegistry::poll_inventory()`] for details.
    pub fn new(registry: &'a mut InventoryRegistry) -> Self {
        Self { registry }
    }
}

impl Future for Update<'_> {
    type Output = Result<(), BoxError>;

    /// A future that returns when the next inventory update or rotation has been completed.
    ///
    /// See [`InventoryRegistry::poll_inventory()`] for details.
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.registry.poll_inventory(cx)
    }
}
