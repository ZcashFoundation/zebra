//! Fixed test vectors for the inventory registry.

use tokio::sync::broadcast;

use zebra_chain::block;

use crate::{
    peer_set::{
        inventory_registry::{InventoryRegistry, InventoryStatus},
        InventoryChange,
    },
    protocol::external::InventoryHash,
};

/// Check an empty inventory registry works as expected.
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

/// Check inventory registration for one advertised hash/peer.
#[tokio::test]
async fn inv_registry_one_advertised_ok() {
    let test_hash = InventoryHash::Block(block::Hash([0; 32]));
    let test_peer = "1.1.1.1:1"
        .parse()
        .expect("unexpected invalid peer address");
    let test_change = InventoryStatus::new_advertised(test_hash, test_peer);

    let (mut inv_registry, inv_stream_tx) = new_inv_registry();

    let receiver_count = inv_stream_tx
        .send(test_change)
        .expect("unexpected failed inventory status send");
    assert_eq!(receiver_count, 1);

    inv_registry
        .update()
        .await
        .expect("unexpected dropped registry sender channel");

    assert_eq!(
        inv_registry.advertising_peers(test_hash).next(),
        Some(&test_peer),
    );
    assert_eq!(inv_registry.missing_peers(test_hash).count(), 0);
}

/// Check inventory registration for one missing hash/peer.
#[tokio::test]
async fn inv_registry_one_missing_ok() {
    let test_hash = InventoryHash::Block(block::Hash([0; 32]));
    let test_peer = "1.1.1.1:1"
        .parse()
        .expect("unexpected invalid peer address");
    let test_change = InventoryStatus::new_missing(test_hash, test_peer);

    let (mut inv_registry, inv_stream_tx) = new_inv_registry();

    let receiver_count = inv_stream_tx
        .send(test_change)
        .expect("unexpected failed inventory status send");
    assert_eq!(receiver_count, 1);

    inv_registry
        .update()
        .await
        .expect("unexpected dropped registry sender channel");

    assert_eq!(inv_registry.advertising_peers(test_hash).count(), 0);
    assert_eq!(
        inv_registry.missing_peers(test_hash).next(),
        Some(&test_peer),
    );
}

/// Returns a newly initialised inventory registry, and a sender for its inventory channel.
fn new_inv_registry() -> (InventoryRegistry, broadcast::Sender<InventoryChange>) {
    let (inv_stream_tx, inv_stream_rx) = broadcast::channel(1);

    let inv_registry = InventoryRegistry::new(inv_stream_rx);

    (inv_registry, inv_stream_tx)
}
