//! Early relay retention and verifier lifetime contracts.

use super::*;
use crate::{ChainTipBlock, ChainTipSender, CheckpointVerifiedBlock};
use zebra_chain::{
    parameters::Network,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};

fn cache() -> (
    Arc<EarlyRelayState>,
    EarlyRelayCache,
    ChainTipSender,
    Arc<Block>,
) {
    let parent: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let (tip, latest, _) = ChainTipSender::new(
        Some(ChainTipBlock::from(CheckpointVerifiedBlock::from(parent))),
        &Network::Mainnet,
    );
    let state = EarlyRelayState::new(Some(latest));
    let cache = state.cache();
    (state, cache, tip, block)
}

fn guard(state: &Arc<EarlyRelayState>, hash: block::Hash) -> EarlyRelayGuard {
    EarlyRelayGuard {
        alive: watch::channel(Some(Arc::new(
            state.slot.clone().try_acquire_owned().unwrap(),
        )))
        .0,
        state: state.clone(),
        hash,
    }
}

#[tokio::test(start_paused = true)]
async fn early_relay_cache_expires_and_cancels() {
    let (state, cache, _, block) = cache();
    let mut changes = cache.subscribe();
    let lease = guard(&state, block.hash());
    state.publish(block.clone(), &lease, 1);
    assert_eq!(*changes.borrow_and_update(), Some(block.hash()));
    assert!(Arc::ptr_eq(&cache.get(&block.hash()).unwrap(), &block));
    // Re-publication must not extend the deadline or emit duplicate advertisements.
    tokio::time::advance(LIFETIME / 2).await;
    state.publish(block.clone(), &lease, 1);
    assert!(!changes.has_changed().unwrap());
    tokio::time::advance(LIFETIME / 2).await;
    assert!(cache.get(&block.hash()).is_none());
    assert_eq!(*changes.borrow_and_update(), None);
    assert!(state.slot.clone().try_acquire_owned().is_err());
    drop(lease);
    tokio::task::yield_now().await;

    let lease = guard(&state, block.hash());
    state.publish(block.clone(), &lease, 1);
    assert!(cache.get(&block.hash()).is_some());
    drop(lease);
    assert!(
        cache.get(&block.hash()).is_none(),
        "cancellation is visible without a task wakeup"
    );
    assert_eq!(*changes.borrow_and_update(), None);
    assert_eq!(
        Arc::strong_count(&block),
        1,
        "withdrawal releases the cached body"
    );
}

/// Withdrawal cannot buy repeated fanout, but a new committed tip releases admission immediately.
#[tokio::test(start_paused = true)]
async fn early_relay_rejection_bounds_replay_until_deadline_or_tip_change() {
    for advance_tip in [false, true] {
        let (state, cache, mut tip, block) = cache();
        let lease = guard(&state, block.hash());
        state.publish(block.clone(), &lease, 1);
        drop(lease);
        assert!(cache.get(&block.hash()).is_none());
        tokio::task::yield_now().await;
        assert_eq!(Arc::strong_count(&block), 1, "withdrawal retains no body");
        assert!(
            state.slot.clone().try_acquire_owned().is_err(),
            "replaying a rejected body must not immediately obtain another advertisement"
        );
        if advance_tip {
            tip.set_best_non_finalized_tip(ChainTipBlock::from(CheckpointVerifiedBlock::from(
                block,
            )));
        } else {
            tokio::time::advance(LIFETIME).await;
        }
        let _permit =
            tokio::time::timeout(Duration::from_secs(1), state.slot.clone().acquire_owned())
                .await
                .expect("deadline or a new committed tip releases replay admission")
                .unwrap();
    }
}

#[tokio::test]
async fn early_relay_cache_bounds_and_stale_parent() {
    let (state, cache, mut tip, block) = cache();
    let lease = guard(&state, block.hash());
    state.publish(block.clone(), &lease, MAX_RETAINED_BYTES + 1);
    assert!(cache.get(&block.hash()).is_none());
    // A small wire body with overallocated decoded transaction storage also exceeds the charge.
    let mut oversized = (*block).clone();
    oversized
        .transactions
        .reserve(MAX_RETAINED_BYTES / size_of::<Arc<zebra_chain::transaction::Transaction>>());
    let (_, charge) = crate::request::block_wire_and_retained_bytes(&oversized, &[]);
    assert!(charge > MAX_RETAINED_BYTES);
    state.publish(Arc::new(oversized), &lease, charge);
    assert!(cache.get(&block.hash()).is_none());
    state.publish(block.clone(), &lease, MAX_RETAINED_BYTES);
    assert!(cache.get(&block.hash()).is_some());
    let mut other = (*block).clone();
    Arc::make_mut(&mut other.header).nonce = [7; 32].into();
    let other = Arc::new(other);
    state.publish(other.clone(), &lease, 1);
    assert!(
        cache.get(&other.hash()).is_none(),
        "one slot cannot retain competitors"
    );
    assert!(cache.get(&block.hash()).is_some());
    tip.set_best_non_finalized_tip(ChainTipBlock::from(CheckpointVerifiedBlock::from(
        block.clone(),
    )));
    assert!(
        cache.get(&block.hash()).is_none(),
        "committed state must now serve the body"
    );
}

#[tokio::test]
async fn early_relay_subscribers_enable_without_mining() {
    let (state, cache, _, block) = cache();
    let lease = guard(&state, block.hash());
    let changes = cache.subscribe();
    drop(cache);
    state.publish(block.clone(), &lease, 1);
    assert_eq!(*changes.borrow(), Some(block.hash()));
    drop(changes);
    drop(lease);
    // The receiver count, not state ownership, is the opt-in switch.
    assert_eq!(state.changes.receiver_count(), 0);
    let state = EarlyRelayState::new(None);
    let lease = guard(&state, block.hash());
    state.publish(block, &lease, 1);
    assert!(state.entry.lock().unwrap().is_none());
}

/// Full blocks of compact transparent payments must fit the decoded-body charge.
#[test]
fn early_relay_retains_large_small_transaction_blocks() {
    use zebra_chain::{
        amount::{Amount, NonNegative},
        transaction::{LockTime, Transaction},
        transparent,
    };
    let transaction = Arc::new(Transaction::test_v4(
        vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: [1; 32].into(),
                index: 0,
            },
            unlock_script: transparent::Script::new(&[]),
            sequence: u32::MAX,
        }],
        vec![transparent::Output::new(
            Amount::<NonNegative>::try_from(1).unwrap(),
            transparent::Script::new(&[0; 25]),
        )],
        LockTime::unlocked(),
        block::Height(1),
    ));
    let mut block: Block = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let count = (usize::try_from(block::MAX_BLOCK_BYTES).unwrap()
        - block.header.zcash_serialized_size()
        - 9)
        / transaction.zcash_serialized_size();
    assert!(count >= 18_000);
    // Duplicate Arcs model layout and content charging only, not consensus validity.
    block.transactions = vec![transaction; count];
    assert!(u64::try_from(block.zcash_serialized_size()).unwrap() <= block::MAX_BLOCK_BYTES);
    assert!(crate::request::block_wire_and_retained_bytes(&block, &[]).1 <= MAX_RETAINED_BYTES);
}

#[tokio::test(start_paused = true)]
async fn early_relay_rejections_have_fixed_count_and_unextended_expiry() {
    let state = EarlyRelayState::new(None);
    let lease = |byte| EarlyRelayGuard {
        alive: watch::Sender::new(None),
        state: state.clone(),
        hash: block::Hash([byte; 32]),
    };
    let first = lease(0);
    first.reject();
    tokio::time::advance(REJECTION_LIFETIME / 2).await;
    first.reject();
    tokio::time::advance(REJECTION_LIFETIME / 2).await;
    assert!(!first.is_rejected(), "replay must not refresh a rejection");

    first.reject();
    for byte in 1..u8::try_from(MAX_REJECTIONS).unwrap() {
        lease(byte).reject();
    }
    assert!(first.is_rejected(), "all eight hashes fit");
    lease(u8::try_from(MAX_REJECTIONS).unwrap()).reject();
    assert!(!first.is_rejected(), "the ninth hash evicts the oldest");
    for byte in 1..=u8::try_from(MAX_REJECTIONS).unwrap() {
        assert!(lease(byte).is_rejected());
    }
    tokio::time::advance(REJECTION_LIFETIME).await;
    for byte in 0..=u8::try_from(MAX_REJECTIONS).unwrap() {
        assert!(!lease(byte).is_rejected());
    }
    assert!(
        state.entry.lock().unwrap().is_none(),
        "hashes retain no bodies"
    );
}

#[tokio::test]
async fn early_relay_rejection_withdraws_live_body_and_prevents_republication() {
    let (state, cache, _, block) = cache();
    let lease = guard(&state, block.hash());
    state.publish(block.clone(), &lease, 1);
    assert!(cache.get(&block.hash()).is_some());
    let concurrent = EarlyRelayGuard {
        alive: watch::Sender::new(None),
        state: state.clone(),
        hash: block.hash(),
    };
    concurrent.reject();
    assert!(cache.get(&block.hash()).is_none());
    state.publish(block.clone(), &lease, 1);
    assert!(cache.get(&block.hash()).is_none());
    assert_eq!(*cache.subscribe().borrow(), None);
    assert_eq!(Arc::strong_count(&block), 1);
}
