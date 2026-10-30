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
#[tokio::test(start_paused = true)]
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

/// Check that failed peers are recorded, aren't cleared by advertisements, and expire after two
/// rotations.
#[tokio::test]
async fn inv_registry_failed_peers_ok() {
    let test_hash = InventoryHash::Block(block::Hash([0; 32]));
    let test_peer = "1.1.1.1:1"
        .parse()
        .expect("unexpected invalid peer address");

    let (mut inv_registry, inv_stream_tx) = new_inv_registry();

    inv_registry.register_failed(test_hash, test_peer);
    assert_eq!(
        inv_registry.failed_peers(test_hash).next(),
        Some(&test_peer)
    );

    // Advertising the inventory again doesn't clear the failure.
    inv_stream_tx
        .send(InventoryStatus::new_available(test_hash, test_peer))
        .expect("unexpected failed inventory status send");
    inv_registry
        .update()
        .await
        .expect("unexpected dropped registry sender channel");

    assert_eq!(
        inv_registry.failed_peers(test_hash).next(),
        Some(&test_peer)
    );
    assert_eq!(inv_registry.missing_peers(test_hash).count(), 0);

    // Failures expire after two rotations.
    inv_registry.rotate();
    assert_eq!(
        inv_registry.failed_peers(test_hash).next(),
        Some(&test_peer)
    );

    inv_registry.rotate();
    assert_eq!(inv_registry.failed_peers(test_hash).count(), 0);
}

/// Check that failed peers are limited to [`MAX_PEERS_PER_INV`] per hash, and
/// [`MAX_INV_PER_MAP`] hashes, keeping the newest entries.
#[tokio::test]
async fn inv_registry_failed_peers_limit() {
    let (mut inv_registry, _inv_stream_tx) = new_inv_registry();

    let test_hash = InventoryHash::Block(block::Hash([0; 32]));
    let test_peers: Vec<PeerSocketAddr> = (1..=MAX_PEERS_PER_INV + 1)
        .map(|port| {
            SocketAddr::new(
                [1, 1, 1, 1].into(),
                port.try_into().expect("port fits in u16"),
            )
            .into()
        })
        .collect();

    for &peer in &test_peers {
        inv_registry.register_failed(test_hash, peer);
    }

    assert_eq!(
        inv_registry.failed_peers(test_hash).count(),
        MAX_PEERS_PER_INV
    );
    assert!(
        !inv_registry
            .failed_peers(test_hash)
            .any(|peer| *peer == test_peers[0]),
        "the oldest failed peer should be removed",
    );

    let test_peer = test_peers[0];
    for index in 0..=MAX_INV_PER_MAP {
        let index = u32::try_from(index).expect("index fits in u32");
        let mut hash = [0; 32];
        hash[..4].copy_from_slice(&index.to_le_bytes());

        inv_registry.register_failed(InventoryHash::Block(block::Hash(hash)), test_peer);
    }

    assert_eq!(inv_registry.failed_current.len(), MAX_INV_PER_MAP);
    assert_eq!(
        inv_registry.failed_peers(test_hash).count(),
        0,
        "the oldest failed hash should be removed",
    );
}

/// N12: refreshing a tracked hash must protect it from eviction at the hash cap.
#[tokio::test]
async fn repeated_failure_refreshes_hash_eviction_order() {
    let (mut registry, _sender) = new_inv_registry();
    let peer = "1.1.1.1:1".parse().expect("the test peer address is valid");
    let hash = |index: usize| {
        let mut bytes = [0; 32];
        bytes[..4].copy_from_slice(
            &u32::try_from(index)
                .expect("the inventory cap fits in u32")
                .to_le_bytes(),
        );
        InventoryHash::Block(block::Hash(bytes))
    };
    for index in 0..MAX_INV_PER_MAP {
        registry.register_failed(hash(index), peer);
    }
    registry.register_failed(hash(0), peer);
    registry.register_failed(hash(MAX_INV_PER_MAP), peer);

    assert_eq!(registry.failed_current.len(), MAX_INV_PER_MAP);
    assert_eq!(
        registry.failed_peers(hash(0)).copied().collect::<Vec<_>>(),
        vec![peer],
        "a repeated failure refreshes the oldest tracked hash",
    );
    assert_eq!(
        registry.failed_peers(hash(1)).count(),
        0,
        "the least recently failed hash is evicted instead",
    );
    assert_eq!(
        registry
            .failed_peers(hash(MAX_INV_PER_MAP))
            .copied()
            .collect::<Vec<_>>(),
        vec![peer],
    );
}

/// N12: refreshing a tracked peer must protect it from eviction at the per-hash cap.
#[tokio::test]
async fn repeated_failure_refreshes_peer_eviction_order() {
    let (mut registry, _sender) = new_inv_registry();
    let hash = InventoryHash::Block(block::Hash([0; 32]));
    let peer = |index: usize| -> PeerSocketAddr {
        SocketAddr::new(
            [1, 1, 1, 1].into(),
            u16::try_from(index + 1).expect("the peer cap fits in a port"),
        )
        .into()
    };
    for index in 0..MAX_PEERS_PER_INV {
        registry.register_failed(hash, peer(index));
    }
    registry.register_failed(hash, peer(0));
    registry.register_failed(hash, peer(MAX_PEERS_PER_INV));

    let failed: Vec<_> = registry.failed_peers(hash).copied().collect();
    assert_eq!(failed.len(), MAX_PEERS_PER_INV);
    assert!(
        failed.contains(&peer(0)),
        "a repeated failure refreshes the oldest peer"
    );
    assert!(
        !failed.contains(&peer(1)),
        "the least recently failed peer is evicted instead"
    );
    assert!(failed.contains(&peer(MAX_PEERS_PER_INV)));
}

/// Missing inventory cannot erase the first announcer credited when a block is committed.
#[tokio::test]
async fn first_compact_source_announcement_is_promoted_only_after_commit() {
    for rotate in [false, true] {
        let (mut registry, _sender) = new_inv_registry();
        let peers: Vec<PeerSocketAddr> = (1..=4)
            .map(|ip| SocketAddr::from(([127, 0, 0, ip], 8233)).into())
            .collect();
        let mut preferences = Vec::new();
        for peer in &peers {
            let (sender, receiver) = tokio::sync::watch::channel(None);
            registry.add_compact_source(*peer, sender);
            preferences.push(receiver);
        }
        registry.select_compact_sources(None, |_| true);
        assert!(preferences[3].borrow().is_none());

        let hash = InventoryHash::Block(block::Hash([1; 32]));
        registry.register(InventoryStatus::new_missing(hash, peers[0]));
        registry.register(InventoryStatus::new_available(hash, peers[3]));
        registry.select_compact_sources(None, |_| true);
        assert!(preferences[3].borrow().is_none());
        registry.register(InventoryStatus::new_available(hash, peers[2]));
        if rotate {
            registry.rotate();
        }
        registry.select_compact_sources(Some(block::Hash([1; 32])), |_| true);
        assert!(preferences[3].borrow().is_some());
        assert!(preferences[2].borrow().is_none());
        assert_eq!(
            registry.missing_peers(hash).copied().collect::<Vec<_>>(),
            vec![peers[0]]
        );

        // A missing status cannot erase an announcement in either registry generation.
        if rotate {
            registry.rotate();
        }
        registry.register(InventoryStatus::new_missing(hash, peers[3]));
        registry.register(InventoryStatus::new_available(hash, peers[2]));
        registry.select_compact_sources(Some(block::Hash([1; 32])), |_| true);
        assert!(preferences[2].borrow().is_none());

        // Sticky missing status must not give repeated announcements fresh source-selection credit.
        let missing_hash = InventoryHash::Block(block::Hash([2; 32]));
        registry.register(InventoryStatus::new_missing(missing_hash, peers[2]));
        registry.register(InventoryStatus::new_available(missing_hash, peers[2]));
        registry.select_compact_sources(Some(block::Hash([2; 32])), |_| true);
        assert!(preferences[2].borrow().is_some());
        for index in [0, 1, 3] {
            let hash = block::Hash([u8::try_from(index + 3).unwrap(); 32]);
            registry.register(InventoryStatus::new_available(
                InventoryHash::Block(hash),
                peers[index],
            ));
            registry.select_compact_sources(Some(hash), |_| true);
        }
        assert!(preferences[2].borrow().is_none());
        registry.register(InventoryStatus::new_available(missing_hash, peers[2]));
        registry.select_compact_sources(Some(block::Hash([6; 32])), |_| true);
        assert!(preferences[2].borrow().is_none());
        assert_eq!(
            registry
                .missing_peers(missing_hash)
                .copied()
                .collect::<Vec<_>>(),
            vec![peers[2]]
        );
    }
}

/// Announcement history has the registry's hash cap and survives at most two idle generations.
#[tokio::test]
async fn compact_announcement_history_is_bounded_and_rotates() {
    let (mut registry, _sender) = new_inv_registry();
    let peer = "1.1.1.1:1".parse().unwrap();
    let hash = |index: usize| {
        let mut bytes = [0; 32];
        bytes[..4].copy_from_slice(&u32::try_from(index).unwrap().to_le_bytes());
        block::Hash(bytes)
    };
    for index in 0..=MAX_INV_PER_MAP {
        registry.register(InventoryStatus::new_available(
            InventoryHash::Block(hash(index)),
            peer,
        ));
    }
    assert_eq!(registry.announced_current.len(), MAX_INV_PER_MAP);
    assert!(!registry.announced_current.contains_key(&hash(0)));
    registry.rotate();
    assert!(registry.announced_current.is_empty());
    assert_eq!(registry.announced_prev.len(), MAX_INV_PER_MAP);

    // Repeated announcements remain remembered through the next interval without earning credit.
    registry.register(InventoryStatus::new_available(
        InventoryHash::Block(hash(MAX_INV_PER_MAP)),
        peer,
    ));
    registry.rotate();
    assert!(registry.announced_current.is_empty());
    assert_eq!(registry.announced_prev.len(), 1);
    assert!(registry.announced_prev.contains_key(&hash(MAX_INV_PER_MAP)));
    registry.rotate();
    assert!(registry.announced_current.is_empty());
    assert!(registry.announced_prev.is_empty());
}

#[tokio::test]
async fn arbitrary_inventory_cannot_replace_compact_sources() {
    let (mut registry, _sender) = new_inv_registry();
    let peers: Vec<PeerSocketAddr> = (1..=6)
        .map(|ip| SocketAddr::from(([127, 0, 0, ip], 8233)).into())
        .collect();
    let mut preferences = Vec::new();
    for peer in &peers {
        let (sender, receiver) = tokio::sync::watch::channel(None);
        registry.add_compact_source(*peer, sender);
        preferences.push(receiver);
    }
    registry.select_compact_sources(None, |_| true);
    for index in 0..=MAX_INV_PER_MAP {
        let mut hash = [0; 32];
        hash[..4].copy_from_slice(&u32::try_from(index).unwrap().to_le_bytes());
        registry.register(InventoryStatus::new_available(
            InventoryHash::Block(block::Hash(hash)),
            peers[3 + index % 3],
        ));
        registry.select_compact_sources(None, |_| true);
        assert_eq!(
            preferences
                .iter()
                .map(|source| source.borrow().is_some())
                .collect::<Vec<_>>(),
            vec![true, true, true, false, false, false],
        );
    }
}
