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
    assert!(state.slot.clone().try_acquire_owned().is_err());
    tokio::time::advance(LIFETIME).await;
    let _permit = tokio::time::timeout(Duration::from_secs(1), state.slot.clone().acquire_owned())
        .await
        .unwrap()
        .unwrap();
}
