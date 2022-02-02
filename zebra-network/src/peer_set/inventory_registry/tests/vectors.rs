//! Fixed test vectors for the inventory registry.

use tokio::sync::broadcast;

use crate::{
    peer_set::{inventory_registry::InventoryRegistry, InventoryChange},
    protocol::external::InventoryHash,
};

/// Make sure an empty inventory registry works as expected.
#[tokio::test]
async fn inv_registry_empty_ok() {
    let fake_hash = InventoryHash::Error;

    let (mut inv_registry, _inv_stream_tx) = new_inv_registry();

    inv_registry
        .update()
        .await
        .expect("unexpected dropped registry sender channel");

    assert_eq!(inv_registry.advertising_peers(fake_hash).count(), 0);
    assert_eq!(inv_registry.missing_peers(fake_hash).count(), 0);
}

/// Returns a newly initialised inventory registry, and a sender for its inventory channel.
fn new_inv_registry() -> (InventoryRegistry, broadcast::Sender<InventoryChange>) {
    let (inv_stream_tx, inv_stream_rx) = broadcast::channel(1);

    let inv_registry = InventoryRegistry::new(inv_stream_rx);

    (inv_registry, inv_stream_tx)
}
