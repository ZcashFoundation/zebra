//! Tests for the inventory registry.

#![allow(clippy::unwrap_in_result)]

use tokio::sync::broadcast;

use crate::peer_set::inventory_registry::{InventoryChange, InventoryRegistry};

mod prop;
mod vectors;

/// The number of changes that can be pending in the inventory channel, before it starts lagging.
///
/// Lagging drops messages, so tests should avoid filling the channel.
pub const MAX_PENDING_CHANGES: usize = 32;

/// Returns a newly initialised inventory registry, and a sender for its inventory channel.
fn new_inv_registry() -> (InventoryRegistry, broadcast::Sender<InventoryChange>) {
    let (inv_stream_tx, inv_stream_rx) = broadcast::channel(MAX_PENDING_CHANGES);

    let inv_registry = InventoryRegistry::new(inv_stream_rx);

    (inv_registry, inv_stream_tx)
}
