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
    #[allow(dead_code)]
    pub(super) fn new(registry: &'a mut InventoryRegistry) -> Self {
        Self { registry }
    }
}

impl Future for Update<'_> {
    type Output = Result<(), BoxError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // TODO: should the future wait until new changes arrive?
        //       or for the rotation timer?
        Poll::Ready(self.registry.poll_inventory(cx))
    }
}
