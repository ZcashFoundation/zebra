//! Randomised property tests for the inventory registry.

use std::{
    collections::HashSet,
    task::{Context, Poll},
};

use futures::task::noop_waker;
use proptest::prelude::*;

use crate::{
    peer::{register_inventory_status, ConnectedAddr},
    peer_set::inventory_registry::{
        tests::{new_inv_registry, MAX_PENDING_CHANGES},
        InventoryMarker,
    },
    protocol::external::{InventoryHash, Message},
    PeerSocketAddr,
};

use InventoryHash::*;

/// The maximum number of random hashes we want to use in these tests.
pub const MAX_HASH_COUNT: usize = 5;

proptest! {
    /// Check inventory registration works via the inbound peer message channel wrapper.
    #[test]
    fn inv_registry_inbound_wrapper_ok(
        status in any::<InventoryMarker>(),
        test_hashes in prop::collection::hash_set(any::<InventoryHash>(), 0..=MAX_HASH_COUNT)
    ) {
        prop_assert!(MAX_HASH_COUNT <= MAX_PENDING_CHANGES, "channel must not block in tests");

        // Start the runtime
        let (runtime, _init_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            // Check all combinations of:
            //
            // Inventory availability:
            // - Available
            // - Missing
            //
            // Inventory multiplicity:
            // - Empty messages are ignored without errors
            // - Single is handled correctly
            // - Multiple are handled correctly
            //
            // And inventory variants:
            // - Block (empty and single only)
            // - Tx for legacy v4 and earlier transactions
            // - Wtx for v5 and later transactions
            //
            // Using randomised proptest inventory data.
            inv_registry_inbound_wrapper_with(status, test_hashes).await;
        })
    }
}

/// Check inventory registration works via the inbound peer message channel wrapper.
#[tracing::instrument]
async fn inv_registry_inbound_wrapper_with(
    status: InventoryMarker,
    test_hashes: HashSet<InventoryHash>,
) {
    let test_peer: PeerSocketAddr = "1.1.1.1:1"
        .parse()
        .expect("unexpected invalid peer address");
    let test_peer = ConnectedAddr::new_inbound_direct(test_peer);

    let test_hashes: Vec<InventoryHash> = test_hashes.into_iter().collect();
    let test_msg = if status.is_available() {
        Message::Inv(test_hashes.clone())
    } else {
        Message::NotFound(test_hashes.clone())
    };

    let (mut inv_registry, inv_stream_tx) = new_inv_registry();

    let forwarded_msg =
        register_inventory_status(Ok(test_msg.clone()), test_peer, inv_stream_tx.clone()).await;
    assert_eq!(
        test_msg.clone(),
        forwarded_msg.expect("unexpected forwarded error result"),
    );

    // We don't actually care if the registry takes any action here.
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    let _poll_pending_or_ok: Poll<()> = inv_registry
        .poll_inventory(&mut cx)
        .map(|result| result.expect("unexpected error polling inventory"));

    let test_peer = test_peer
        .get_transient_addr()
        .expect("unexpected None: expected Some transient peer address");

    for &test_hash in test_hashes.iter() {
        // The registry should ignore these unsupported types.
        // (Currently, it panics if they are registered, but they are ok to query.)
        if matches!(test_hash, Error | FilteredBlock(_)) {
            assert_eq!(inv_registry.advertising_peers(test_hash).count(), 0);
            assert_eq!(inv_registry.missing_peers(test_hash).count(), 0);
            continue;
        }

        if status.is_available() {
            // register_inventory_status doesn't register multi-block available inventory,
            // for performance reasons.
            if test_hashes.len() > 1 && test_hash.block_hash().is_some() {
                assert_eq!(inv_registry.advertising_peers(test_hash).count(), 0);
                assert_eq!(inv_registry.missing_peers(test_hash).count(), 0);
                continue;
            }

            assert_eq!(
                inv_registry.advertising_peers(test_hash).next(),
                Some(&test_peer),
                "unexpected None hash: {:?},\n\
                 in message {:?} \n\
                 with length {}\n\
                 should be advertised\n",
                test_hash,
                test_msg,
                test_hashes.len(),
            );
            assert_eq!(inv_registry.advertising_peers(test_hash).count(), 1);
            assert_eq!(inv_registry.missing_peers(test_hash).count(), 0);
        } else {
            assert_eq!(inv_registry.advertising_peers(test_hash).count(), 0);
            assert_eq!(
                inv_registry.missing_peers(test_hash).next(),
                Some(&test_peer),
                "unexpected None hash: {:?},\n\
                 in message {:?} \n\
                 with length {}\n\
                 should be advertised\n",
                test_hash,
                test_msg,
                test_hashes.len(),
            );
            assert_eq!(inv_registry.missing_peers(test_hash).count(), 1);
        }
    }
}
