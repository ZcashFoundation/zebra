//! Early relay retention and verifier lifetime contracts.

use super::*;
use crate::{arbitrary::Prepare, ChainTipBlock, ChainTipSender};
use zebra_chain::{
    parameters::Network,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};

fn cache() -> (
    Arc<EarlyRelayState>,
    EarlyRelayCache,
    watch::Sender<NonFinalizedState>,
    Arc<Block>,
) {
    let (finalized, mut non_finalized, _) = crate::tests::setup::new_state_with_mainnet_genesis();
    let parent: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .unwrap();
    non_finalized
        .commit_new_chain(parent.prepare(), &finalized.db)
        .unwrap();
    let (tip, snapshot) = watch::channel(non_finalized);
    let state = EarlyRelayState::new(WatchReceiver::new(snapshot));
    let cache = state.cache();
    (state, cache, tip, block)
}

fn guard(state: &Arc<EarlyRelayState>, hash: block::Hash) -> EarlyRelayGuard {
    let mut context = state.snapshot.clone();
    context.mark_as_seen();
    EarlyRelayGuard {
        alive: watch::Sender::new(()),
        state: state.clone(),
        hash,
        context,
        height: block::Height(2),
        origin: EarlyRelayOrigin::Peer,
    }
}

#[tokio::test]
async fn early_relay_rechecks_context_after_authentication_and_publication() {
    for update_before_publication in [false, true] {
        let (state, cache, tip, block) = cache();
        let lease = guard(&state, block.hash());
        if !update_before_publication {
            state.publish(block.clone(), &lease, 1);
            assert!(cache.get(&block.hash()).is_some());
        }

        // Model a commit/invalidation publication that restores the same parent hash.
        let parent = state.best_tip_hash();
        tip.send_modify(|_| {});
        assert_eq!(state.best_tip_hash(), parent);
        if update_before_publication {
            state.publish(block.clone(), &lease, 1);
        }
        assert!(
            cache.get(&block.hash()).is_none(),
            "a restored parent does not make stale authentication current again"
        );
        assert_eq!(*cache.subscribe().borrow(), None);
    }
}

#[tokio::test(start_paused = true)]
async fn early_relay_cache_expires_and_cancels() {
    // A dropped tip sender would wake the expiry task immediately and withdraw the body.
    let (state, cache, _tip, block) = cache();
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
    tokio::task::yield_now().await;
    assert!(state.slot.clone().try_acquire_owned().is_ok());
    drop(lease);

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
        let (state, cache, tip, block) = cache();
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
            tip.send_replace(NonFinalizedState::new(&Network::Mainnet));
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
    let (state, cache, tip, block) = cache();
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
    tip.send_replace(NonFinalizedState::new(&Network::Mainnet));
    assert!(
        cache.get(&block.hash()).is_none(),
        "committed state must now serve the body"
    );
}

#[tokio::test]
async fn early_relay_subscribers_enable_without_mining() {
    let (state, cache, _tip, block) = cache();
    let lease = guard(&state, block.hash());
    let changes = cache.subscribe();
    drop(cache);
    state.publish(block.clone(), &lease, 1);
    assert_eq!(*changes.borrow(), Some(block.hash()));
    drop(changes);
    drop(lease);
    // The receiver count, not state ownership, is the opt-in switch.
    assert_eq!(state.changes.receiver_count(), 0);
    let state = EarlyRelayState::new(WatchReceiver::new(
        watch::channel(NonFinalizedState::new(&Network::Mainnet)).1,
    ));
    let lease = guard(&state, block.hash());
    state.publish(block, &lease, 1);
    assert!(state.entry.lock().unwrap().is_none());
}

/// Recipient demand is sampled before authentication, without changing mandatory inbound checks.
#[test]
fn early_relay_skips_authentication_without_recipients() {
    let _init_guard = zebra_test::init();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            let network = Network::new_regtest(
                zebra_chain::parameters::testnet::ConfiguredActivationHeights {
                    nu5: Some(1),
                    ..Default::default()
                }
                .into(),
            );
            let (_state, read, _, _) =
                crate::init(crate::Config::ephemeral(), &network, block::Height(0), 0).await;
            let cache = read.early_relay.cache();
            let (recipients, demand) = watch::channel(false);
            cache.set_recipients(demand);
            let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
                .zcash_deserialize_into()
                .unwrap();
            // Holding admission distinguishes a skipped request from one that authenticates.
            let _permit = read
                .early_relay
                .computation
                .clone()
                .acquire_owned()
                .await
                .unwrap();
            let skipped = read
                .clone()
                .early_relay(block.clone(), false, true, EarlyRelayOrigin::Peer)
                .await
                .unwrap();
            assert!(skipped.is_none());
            assert!(read.early_relay.entry.lock().unwrap().is_none());

            // A later negotiation enables the next request, not work already skipped.
            recipients.send_replace(true);
            let mut negotiated = Box::pin(read.clone().early_relay(
                block.clone(),
                false,
                true,
                EarlyRelayOrigin::Peer,
            ));
            assert!(futures::poll!(negotiated.as_mut()).is_pending());
            drop(negotiated);

            recipients.send_replace(false);
            let mut required = Box::pin(read.clone().early_relay(
                block.clone(),
                true,
                true,
                EarlyRelayOrigin::Peer,
            ));
            assert!(futures::poll!(required.as_mut()).is_pending());
            drop(required);

            // A queued optional request must recheck demand before entering the blocking pool.
            // Occupy its only worker so accidentally spawning authentication cannot complete.
            let (started, running) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                wait.recv().unwrap();
            });
            running.await.unwrap();
            recipients.send_replace(true);
            let mut queued = Box::pin(read.clone().early_relay(
                block.clone(),
                false,
                true,
                EarlyRelayOrigin::Peer,
            ));
            assert!(futures::poll!(queued.as_mut()).is_pending());
            recipients.send_replace(false);
            drop(_permit);
            let skipped = futures::poll!(queued.as_mut());
            release.send(()).unwrap();
            assert!(matches!(skipped, std::task::Poll::Ready(Ok(None))));
            blocker.await.unwrap();

            // A disconnected producer cannot leave optional authentication permanently enabled.
            recipients.send_replace(true);
            drop(recipients);
            assert!(read
                .clone()
                .early_relay(block, false, true, EarlyRelayOrigin::Peer)
                .await
                .unwrap()
                .is_none());
        })
        .await
        .expect("absent recipients skip authentication without waiting for its permit");
    });
}

#[tokio::test]
async fn early_relay_recipient_demand_controls_retention() {
    let (state, cache, _tip, block) = cache();
    let (recipients, demand) = watch::channel(false);
    cache.set_recipients(demand);
    let lease = guard(&state, block.hash());
    state.publish(block.clone(), &lease, 1);
    assert!(cache.get(&block.hash()).is_none());
    assert_eq!(state.slot.available_permits(), 1);

    recipients.send_replace(true);
    state.publish(block.clone(), &lease, 1);
    assert!(cache.get(&block.hash()).is_some());
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
async fn early_relay_rejections_survive_time_and_competing_hashes() {
    let (state, cache, _snapshot, block) = cache();
    let first = guard(&state, block.hash());
    first.reject();
    for byte in 0..9 {
        guard(&state, block::Hash([byte; 32])).reject();
    }
    tokio::time::advance(Duration::from_secs(61)).await;
    assert!(first.is_rejected());
    let replay = guard(&state, block.hash());
    assert!(replay.is_rejected());
    state.publish(block.clone(), &replay, 1);
    assert!(cache.get(&block.hash()).is_none());
    assert!(
        state.entry.lock().unwrap().is_none(),
        "rejections retain no bodies"
    );
}

#[tokio::test]
async fn early_relay_rejection_withdraws_live_body_and_prevents_republication() {
    let (state, cache, _tip, block) = cache();
    let lease = guard(&state, block.hash());
    state.publish(block.clone(), &lease, 1);
    assert!(cache.get(&block.hash()).is_some());
    let concurrent = guard(&state, block.hash());
    concurrent.reject();
    assert!(cache.get(&block.hash()).is_none());
    state.publish(block.clone(), &lease, 1);
    assert!(cache.get(&block.hash()).is_none());
    assert_eq!(*cache.subscribe().borrow(), None);
    assert_eq!(Arc::strong_count(&block), 1);
}

#[tokio::test]
async fn early_relay_rejection_capacity_fails_closed() {
    use super::super::non_finalized_state::MAX_EARLY_RELAY_REJECTIONS;

    let (state, cache, _snapshot, block) = cache();
    let lease = guard(&state, block.hash());
    state.publish(block.clone(), &lease, 1);
    assert!(cache.get(&block.hash()).is_some());
    for index in 0..=MAX_EARLY_RELAY_REJECTIONS {
        let mut hash = block::Hash([0; 32]);
        hash.0[..4].copy_from_slice(&u32::try_from(index).unwrap().to_le_bytes());
        guard(&state, hash).reject();
    }
    assert!(
        !lease.is_rejected(),
        "capacity must not reject ordinary full validation"
    );
    assert!(!state.allows_early_relay(block.hash()));
    assert!(cache.get(&block.hash()).is_none());
    state.publish(block.clone(), &lease, 1);
    assert!(cache.get(&block.hash()).is_none());
    assert_eq!(*cache.subscribe().borrow(), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn rejection_authentication_does_not_share_normal_admission_or_repeat_known_hashes() {
    let _init_guard = zebra_test::init();
    let (_state, read, _, _) = crate::init(
        crate::Config::ephemeral(),
        &Network::Mainnet,
        block::Height(0),
        0,
    )
    .await;
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let _normal = read
        .early_relay
        .computation
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    assert!(!tokio::time::timeout(
        Duration::from_secs(10),
        read.clone().reject_block(block.clone()),
    )
    .await
    .expect("rejection authentication must not wait for normal admission"));
    read.early_relay.reject(block.hash(), block::Height(2));
    let _rejection = read
        .early_relay
        .rejection_computation
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(10), read.reject_block(block),)
            .await
            .expect("known rejections must not wait for authentication")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rejection_authentication_accepts_a_retained_non_best_parent() {
    let _init_guard = zebra_test::init();
    let (finalized, mut snapshot, _) = crate::tests::setup::new_state_with_mainnet_genesis();
    let parent: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .unwrap();
    snapshot
        .commit_new_chain(parent.prepare(), &finalized.db)
        .unwrap();
    let mut sibling = (*block).clone();
    Arc::make_mut(&mut sibling.header).nonce = [7; 32].into();
    snapshot
        .commit_block(Arc::new(sibling).prepare(), &finalized.db)
        .unwrap();
    assert_ne!(
        snapshot.best_tip().unwrap().1,
        block.header.previous_block_hash
    );
    let (_sender, receiver) = watch::channel(snapshot);
    let read = ReadStateService::new(&finalized, None, WatchReceiver::new(receiver));
    assert!(tokio::time::timeout(
        Duration::from_secs(10),
        read.clone().reject_block(block.clone())
    )
    .await
    .expect("retained-parent rejection authentication must finish"));
    assert!(read
        .early_relay
        .rejections()
        .lock()
        .unwrap()
        .contains(block.hash()));
}

#[tokio::test(start_paused = true)]
async fn early_relay_tip_change_releases_slot_with_live_verifier() {
    let (state, cache, tip, block) = cache();
    let lease = guard(&state, block.hash());
    let mut changes = cache.subscribe();
    state.publish(block.clone(), &lease, 1);
    assert_eq!(*changes.borrow_and_update(), Some(block.hash()));
    tokio::task::yield_now().await;
    tip.send_replace(NonFinalizedState::new(&Network::Mainnet));
    tokio::time::timeout(Duration::from_secs(1), changes.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*changes.borrow(), None);
    let _permit = state.slot.clone().try_acquire_owned().unwrap();
    drop(lease);
}

#[tokio::test(flavor = "multi_thread")]
async fn early_relay_declines_finalized_only_parent() {
    use tower::ServiceExt;
    use zebra_chain::{
        parameters::testnet::ConfiguredActivationHeights,
        transaction::{LockTime, Transaction},
        transparent,
    };

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let network = Network::new_regtest(
            ConfiguredActivationHeights {
                canopy: Some(1),
                nu5: Some(1),
                ..Default::default()
            }
            .into(),
        );
        let (state, read, _, _) =
            crate::init(crate::Config::ephemeral(), &network, block::Height(0), 0).await;
        let state = tower::buffer::Buffer::new(state, 10);
        state
            .clone()
            .oneshot(crate::Request::CommitCheckpointVerifiedBlock(
                block::genesis::regtest_genesis_block().into(),
            ))
            .await
            .unwrap();
        let crate::ReadResponse::ChainInfo(info) = read
            .clone()
            .oneshot(crate::ReadRequest::ChainInfo)
            .await
            .unwrap()
        else {
            panic!("expected chain info")
        };
        let height = info.tip_height.next().unwrap();
        let transactions = vec![Arc::new(Transaction::test_v5(
            NetworkUpgrade::Nu5,
            vec![transparent::Input::Coinbase {
                height,
                data: vec![0],
                sequence: u32::MAX,
            }],
            vec![],
            LockTime::unlocked(),
            height,
        ))];
        let mut block = Block {
            header: Arc::new(block::Header {
                version: 4,
                previous_block_hash: info.tip_hash,
                merkle_root: transactions.iter().map(|tx| tx.hash()).collect(),
                commitment_bytes: [0; 32].into(),
                time: info.cur_time.into(),
                difficulty_threshold: info.expected_difficulty,
                nonce: [0; 32].into(),
                solution: zebra_chain::work::equihash::Solution::Regtest([0; 36]),
            }),
            transactions,
        };
        Arc::make_mut(&mut block.header).commitment_bytes = <[u8; 32]>::from(
            block::ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
                &info
                    .chain_history_root
                    .unwrap_or(block::CHAIN_HISTORY_ACTIVATION_RESERVED.into()),
                &block.auth_data_root(),
            ),
        )
        .into();
        let block = Arc::new(block);
        assert!(read
            .clone()
            .early_relay(block.clone(), true, true, EarlyRelayOrigin::Peer)
            .await
            .unwrap()
            .is_none());
        for request in [
            crate::ReadRequest::CheckBestTipHeader {
                header: block.header.clone(),
                height,
            },
            crate::ReadRequest::CheckBestTipBlockCommitments(block),
        ] {
            assert!(read.clone().oneshot(request).await.is_err());
        }
    })
    .await
    .expect("snapshot admission must finish");
}

/// Both completion channels must observe a published checkpoint tip.
#[test]
fn early_relay_checkpoint_commit_publishes_tip_before_notifications() {
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Wake, Waker},
    };

    use crate::{
        service::write::BlockWriteSender, tests::setup::new_state_with_mainnet_genesis,
        MinedBlockNotifications,
    };

    struct PublishedContextAtWake {
        tip: crate::LatestChainTip,
        observed: std::sync::mpsc::Sender<Option<block::Hash>>,
    }

    impl Wake for PublishedContextAtWake {
        fn wake(self: Arc<Self>) {
            use zebra_chain::chain_tip::ChainTip;
            self.observed.send(self.tip.best_tip_hash()).unwrap();
        }
    }

    let _init_guard = zebra_test::init();
    let (finalized, non_finalized, genesis) = new_state_with_mainnet_genesis();
    let (tip_sender, tip, _) =
        ChainTipSender::new(Some(ChainTipBlock::from(genesis)), &Network::Mainnet);
    let (state_sender, _snapshot) = watch::channel(non_finalized.clone());
    let (sender, _resets, _rejections, worker) = BlockWriteSender::spawn(
        finalized,
        non_finalized,
        tip_sender,
        state_sender,
        true,
        None,
    );
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    let waker = Waker::from(Arc::new(PublishedContextAtWake {
        tip,
        observed: observed_tx,
    }));
    let mut cx = Context::from_waker(&waker);
    let (rsp_tx, mut rsp_rx) = tokio::sync::oneshot::channel();
    assert!(Pin::new(&mut rsp_rx).poll(&mut cx).is_pending());
    let (committed, mut commits) = tokio::sync::mpsc::channel(1);
    assert!(commits.poll_recv(&mut cx).is_pending());
    let (validated, _bodies) = watch::channel(None);
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let hash = block.hash();
    sender
        .finalized
        .as_ref()
        .unwrap()
        .send((
            block.into(),
            rsp_tx,
            Some(MinedBlockNotifications::new(committed, validated)),
        ))
        .unwrap();
    let observations = [
        observed_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        observed_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
    ];
    assert_eq!(commits.try_recv().unwrap(), (hash, block::Height(1)));
    assert_eq!(rsp_rx.try_recv().unwrap().unwrap(), hash);
    drop(sender);
    Arc::try_unwrap(worker.unwrap()).unwrap().join().unwrap();
    for tip in observations {
        assert_eq!(
            tip,
            Some(hash),
            "checkpoint completion must publish the parent"
        );
    }
}
