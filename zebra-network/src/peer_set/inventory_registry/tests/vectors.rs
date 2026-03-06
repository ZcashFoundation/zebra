//! Fixed test vectors for the inventory registry.

use std::{cmp::min, net::SocketAddr};

use zebra_chain::{block, serialization::AtLeastOne, transaction};

use crate::{
    peer_set::inventory_registry::{
        tests::new_inv_registry, InventoryMarker, InventoryStatus, MAX_INV_PER_MAP,
        MAX_PEERS_PER_INV,
    },
    protocol::external::InventoryHash,
    PeerSocketAddr,
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

/// Check inventory registration limits.
#[tokio::test]
async fn inv_registry_limit() {
    inv_registry_limit_for(InventoryMarker::Available(())).await;
    inv_registry_limit_for(InventoryMarker::Missing(())).await;
}

/// Check the inventory registration limit for `status`.
async fn inv_registry_limit_for(status: InventoryMarker) {
    let single_test_hash = InventoryHash::Block(block::Hash([0xbb; 32]));
    let single_test_peer = "1.1.1.1:1"
        .parse()
        .expect("unexpected invalid peer address");

    let (mut inv_registry, inv_stream_tx) = new_inv_registry();

    // Check hash limit
    for hash_count in 0..(MAX_INV_PER_MAP + 10) {
        let mut test_hash = hash_count.to_ne_bytes().to_vec();
        test_hash.resize(32, 0);
        let test_hash = InventoryHash::Tx(transaction::Hash(test_hash.try_into().unwrap()));

        let test_change = status.map(|()| {
            let at_least_one = AtLeastOne::from_vec(vec![test_hash]);

            match at_least_one {
                Ok(at_least_one) => (at_least_one, single_test_peer),
                Err(_) => {
                    panic!("failed to create AtLeastOne")
                }
            }
        });

        let receiver_count = inv_stream_tx
            .send(test_change)
            .expect("unexpected failed inventory status send");

        assert_eq!(receiver_count, 1);

        inv_registry
            .update()
            .await
            .expect("unexpected dropped registry sender channel");

        if status.is_available() {
            assert_eq!(inv_registry.advertising_peers(test_hash).count(), 1);
            assert_eq!(inv_registry.missing_peers(test_hash).count(), 0);
        } else {
            assert_eq!(inv_registry.advertising_peers(test_hash).count(), 0);
            assert_eq!(inv_registry.missing_peers(test_hash).count(), 1);
        }

        // SECURITY: limit inventory memory usage
        assert_eq!(
            inv_registry.status_hashes().count(),
            min(hash_count + 1, MAX_INV_PER_MAP),
        );
    }

    // Check peer address per hash limit
    let (mut inv_registry, inv_stream_tx) = new_inv_registry();

    for peer_count in 0..(MAX_PEERS_PER_INV + 10) {
        let test_peer: PeerSocketAddr = SocketAddr::new(
            "2.2.2.2".parse().unwrap(),
            peer_count.try_into().expect("fits in u16"),
        )
        .into();

        let test_change = status.map(|()| {
            let at_least_one = AtLeastOne::from_vec(vec![single_test_hash]);

            match at_least_one {
                Ok(at_least_one) => (at_least_one, test_peer),
                Err(_) => {
                    panic!("failed to create AtLeastOne")
                }
            }
        });

        let receiver_count = inv_stream_tx
            .send(test_change)
            .expect("unexpected failed inventory status send");

        assert_eq!(receiver_count, 1);

        inv_registry
            .update()
            .await
            .expect("unexpected dropped registry sender channel");

        assert_eq!(inv_registry.status_hashes().count(), 1);

        let limited_count = min(peer_count + 1, MAX_PEERS_PER_INV);

        // SECURITY: limit inventory memory usage
        if status.is_available() {
            assert_eq!(
                inv_registry.advertising_peers(single_test_hash).count(),
                limited_count,
            );
            assert_eq!(inv_registry.missing_peers(single_test_hash).count(), 0);
        } else {
            assert_eq!(inv_registry.advertising_peers(single_test_hash).count(), 0);
            assert_eq!(
                inv_registry.missing_peers(single_test_hash).count(),
                limited_count,
            );
        }
    }
}
