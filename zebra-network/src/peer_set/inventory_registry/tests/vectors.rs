//! Fixed test vectors for the inventory registry.

use zebra_chain::block;

use crate::{
    peer_set::inventory_registry::{tests::new_inv_registry, InventoryStatus},
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
    let test_change = InventoryStatus::new_available(test_hash, test_peer);

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
    assert_eq!(inv_registry.advertising_peers(test_hash).count(), 1);
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
    assert_eq!(inv_registry.missing_peers(test_hash).count(), 1);
}

/// Check inventory registration for one hash/peer prefers missing over advertised.
#[tokio::test]
async fn inv_registry_prefer_missing_ok() {
    inv_registry_prefer_missing_order(true).await;
    inv_registry_prefer_missing_order(false).await;
}

async fn inv_registry_prefer_missing_order(missing_first: bool) {
    let test_hash = InventoryHash::Block(block::Hash([0; 32]));
    let test_peer = "1.1.1.1:1"
        .parse()
        .expect("unexpected invalid peer address");

    let missing_change = InventoryStatus::new_missing(test_hash, test_peer);
    let advertised_change = InventoryStatus::new_available(test_hash, test_peer);

    let (mut inv_registry, inv_stream_tx) = new_inv_registry();

    let changes = if missing_first {
        [missing_change, advertised_change]
    } else {
        [advertised_change, missing_change]
    };

    for change in changes {
        let receiver_count = inv_stream_tx
            .send(change)
            .expect("unexpected failed inventory status send");
        assert_eq!(receiver_count, 1);
    }

    // TODO: also test with updates after each change
    inv_registry
        .update()
        .await
        .expect("unexpected dropped registry sender channel");

    assert_eq!(inv_registry.advertising_peers(test_hash).count(), 0);
    assert_eq!(
        inv_registry.missing_peers(test_hash).next(),
        Some(&test_peer),
    );
    assert_eq!(inv_registry.missing_peers(test_hash).count(), 1);
}

/// Check inventory registration for one hash/peer prefers current over previous.
#[tokio::test]
async fn inv_registry_prefer_current_ok() {
    inv_registry_prefer_current_order(true).await;
    inv_registry_prefer_current_order(false).await;
}

async fn inv_registry_prefer_current_order(missing_current: bool) {
    let test_hash = InventoryHash::Block(block::Hash([0; 32]));
    let test_peer = "1.1.1.1:1"
        .parse()
        .expect("unexpected invalid peer address");

    let missing_change = InventoryStatus::new_missing(test_hash, test_peer);
    let advertised_change = InventoryStatus::new_available(test_hash, test_peer);

    let (mut inv_registry, inv_stream_tx) = new_inv_registry();

    let changes = if missing_current {
        [advertised_change, missing_change]
    } else {
        [missing_change, advertised_change]
    };

    for change in changes {
        // This rotation has no effect in the first loop iteration, because the registry is empty.
        inv_registry.rotate();

        let receiver_count = inv_stream_tx
            .send(change)
            .expect("unexpected failed inventory status send");
        assert_eq!(receiver_count, 1);

        // We must update after each change, so the rotation puts the first change in `prev`.
        inv_registry
            .update()
            .await
            .expect("unexpected dropped registry sender channel");
    }

    if missing_current {
        assert_eq!(inv_registry.advertising_peers(test_hash).count(), 0);
        assert_eq!(
            inv_registry.missing_peers(test_hash).next(),
            Some(&test_peer),
        );
        assert_eq!(inv_registry.missing_peers(test_hash).count(), 1);
    } else {
        assert_eq!(
            inv_registry.advertising_peers(test_hash).next(),
            Some(&test_peer),
        );
        assert_eq!(inv_registry.advertising_peers(test_hash).count(), 1);
        assert_eq!(inv_registry.missing_peers(test_hash).count(), 0);
    }
}
