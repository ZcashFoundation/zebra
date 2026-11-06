//! Atomic publication, rejection, and shared verifier cancellation.

use std::sync::Barrier;

use super::*;

#[test]
fn early_relay_concurrent_rejections_are_all_recorded() {
    const REJECTIONS: u8 = 8;
    let (state, _cache, _tip, _block) = cache();
    let start = Barrier::new(REJECTIONS.into());
    std::thread::scope(|threads| {
        for byte in 0..REJECTIONS {
            let state = &state;
            let start = &start;
            threads.spawn(move || {
                let lease = guard(state, block::Hash([byte; 32]));
                start.wait();
                lease.reject();
            });
        }
    });
    for byte in 0..REJECTIONS {
        assert!(guard(&state, block::Hash([byte; 32])).is_rejected());
    }
}

#[tokio::test]
async fn early_relay_publication_and_rejection_are_atomic() {
    let (state, cache, _tip, block) = cache();
    let lease = guard(&state, block.hash());
    let rejection = guard(&state, block.hash());
    let start = Barrier::new(2);
    let runtime = tokio::runtime::Handle::current();
    std::thread::scope(|threads| {
        threads.spawn(|| {
            let _entered = runtime.enter();
            start.wait();
            state.publish(block.clone(), &lease, 1);
        });
        threads.spawn(|| {
            start.wait();
            rejection.reject();
        });
    });
    assert!(lease.is_rejected());
    assert!(cache.get(&block.hash()).is_none());
    assert_eq!(*cache.subscribe().borrow(), None);
    assert_eq!(Arc::strong_count(&block), 1);
}

/// A paused publisher reserves admission without making same-parent contenders wait on its body.
#[tokio::test]
async fn early_relay_reservation_is_atomic_before_body_publication() {
    let (state, cache, _tip, block) = cache();
    let lease = guard(&state, block.hash());
    let mut other = (*block).clone();
    Arc::make_mut(&mut other.header).nonce = [7; 32].into();
    let other = Arc::new(other);
    let other_hash = other.hash();
    let competitor = guard(&state, other_hash);
    let mut reservations = state.reservation.subscribe();
    // Keep the winner from storing its body after it reserves admission.
    let entry = state.entry.borrow();
    let publisher = tokio::task::spawn_blocking({
        let state = state.clone();
        let block = block.clone();
        let lease = lease.clone();
        move || state.publish(block, &lease, 1)
    });
    let reserved = tokio::time::timeout(Duration::from_secs(10), reservations.changed()).await;
    if !matches!(reserved, Ok(Ok(()))) {
        drop(entry);
        publisher.await.unwrap();
        panic!("the publisher must reserve admission before accessing the retained body");
    }
    let mut contender = tokio::task::spawn_blocking({
        let state = state.clone();
        move || state.publish(other, &competitor, 1)
    });
    let skipped = tokio::time::timeout(Duration::from_secs(10), &mut contender).await;
    drop(entry);
    publisher.await.unwrap();
    match skipped {
        Ok(result) => result.unwrap(),
        Err(_) => {
            contender.await.unwrap();
            panic!("a competing body must skip the reserved slot without waiting for publication");
        }
    }
    assert!(Arc::ptr_eq(&cache.get(&block.hash()).unwrap(), &block));
    assert!(cache.get(&other_hash).is_none());
    assert_eq!(*cache.subscribe().borrow(), Some(block.hash()));
}

/// A delayed publisher cannot outlive its reservation or overwrite a replacement publication.
#[tokio::test(start_paused = true)]
async fn early_relay_delayed_publication_preserves_current_reservation() {
    for replace in [false, true] {
        let (state, cache, _tip, block) = cache();
        let lease = guard(&state, block.hash());
        let alive = lease.alive.subscribe();
        let deadline = tokio::time::Instant::now() + LIFETIME;
        // Pause the publisher immediately after its atomic reservation.
        state.reservation.send_replace(Some(Reservation {
            parent: block.header.previous_block_hash,
            deadline,
            alive: alive.clone(),
        }));
        let pending = Entry {
            block: block.clone(),
            origin: lease.origin,
            hash: block.hash(),
            alive: alive.clone(),
            deadline,
            context: lease.context.clone(),
        };
        tokio::time::advance(LIFETIME).await;
        let mut other = (*block).clone();
        Arc::make_mut(&mut other.header).nonce = [7; 32].into();
        let other = Arc::new(other);
        let replacement = guard(&state, other.hash());
        if replace {
            state.publish(other.clone(), &replacement, 1);
            assert!(cache.get(&other.hash()).is_some());
        }

        // Resume the old publisher after its deadline, with or without a newer reservation.
        assert!(!state.publish_reserved(pending));
        state.notify_published(block.hash(), &alive);
        let expected = replace.then_some(other.hash());
        assert_eq!(*cache.subscribe().borrow(), expected);
        assert!(cache.get(&block.hash()).is_none());
        assert_eq!(cache.get(&other.hash()).is_some(), replace);
        if replace {
            let retry = guard(&state, block.hash());
            state.publish(block.clone(), &retry, 1);
            assert!(cache.get(&other.hash()).is_some());
            assert!(cache.get(&block.hash()).is_none());
            assert_eq!(*cache.subscribe().borrow(), expected);
        }
    }
}

/// Restoring the same parent cannot revive an older publisher or its live authentication guard.
#[tokio::test(start_paused = true)]
async fn early_relay_returned_parent_does_not_revive_delayed_publication() {
    let (state, cache, tip, block) = cache();
    let lease = guard(&state, block.hash());
    let alive = lease.alive.subscribe();
    let deadline = tokio::time::Instant::now() + LIFETIME;
    state.reservation.send_replace(Some(Reservation {
        parent: block.header.previous_block_hash,
        deadline,
        alive: alive.clone(),
    }));
    let pending = Entry {
        block: block.clone(),
        origin: lease.origin,
        hash: block.hash(),
        alive: alive.clone(),
        deadline,
        context: lease.context.clone(),
    };
    let (finalized, _, _) = crate::tests::setup::new_state_with_mainnet_genesis();
    let original = tip.borrow().clone();
    let mut committed = original.clone();
    committed
        .commit_block(block.clone().prepare(), &finalized.db)
        .unwrap();
    tip.send_replace(committed);
    let child: Arc<Block> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS[&3]
        .zcash_deserialize_into()
        .unwrap();
    let child_lease = guard(&state, child.hash());
    state.publish(child.clone(), &child_lease, 1);
    assert!(cache.get(&child.hash()).is_some());

    tip.send_replace(original);
    let mut other = (*block).clone();
    Arc::make_mut(&mut other.header).nonce = [7; 32].into();
    let other = Arc::new(other);
    let replacement = guard(&state, other.hash());
    state.publish(other.clone(), &replacement, 1);
    assert!(cache.get(&other.hash()).is_some());
    assert!(!state.publish_reserved(pending));
    state.notify_published(block.hash(), &alive);
    drop(alive);
    state.publish(block.clone(), &lease, 1);
    assert!(cache.get(&other.hash()).is_some());
    assert!(cache.get(&block.hash()).is_none());
    assert_eq!(*cache.subscribe().borrow(), Some(other.hash()));
}

#[tokio::test(start_paused = true)]
async fn early_relay_only_last_guard_cancellation_withdraws_without_a_reader() {
    let (state, cache, _tip, block) = cache();
    let lease = guard(&state, block.hash());
    let other_verifier = lease.clone();
    let mut changes = cache.subscribe();
    state.publish(block.clone(), &lease, 1);
    changes.borrow_and_update();
    drop(lease);
    assert!(cache.get(&block.hash()).is_some());
    drop(other_verifier);
    tokio::time::timeout(Duration::from_secs(1), changes.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*changes.borrow(), None);
    assert_eq!(Arc::strong_count(&block), 1);
    assert!(state.reservation.borrow().is_some());
    tokio::time::advance(LIFETIME).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while state.reservation.borrow().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
