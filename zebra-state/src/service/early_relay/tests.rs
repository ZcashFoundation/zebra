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
        alive: watch::Sender::new(()),
        state: state.clone(),
        hash,
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
        alive: watch::Sender::new(()),
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
        alive: watch::Sender::new(()),
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

#[tokio::test(start_paused = true)]
async fn early_relay_tip_change_releases_slot_with_live_verifier() {
    let (state, cache, mut tip, block) = cache();
    let lease = guard(&state, block.hash());
    let mut changes = cache.subscribe();
    state.publish(block.clone(), &lease, 1);
    assert_eq!(*changes.borrow_and_update(), Some(block.hash()));
    tokio::task::yield_now().await;
    tip.set_best_non_finalized_tip(ChainTipBlock::from(CheckpointVerifiedBlock::from(block)));
    tokio::time::timeout(Duration::from_secs(1), changes.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*changes.borrow(), None);
    let _permit = state.slot.clone().try_acquire_owned().unwrap();
    drop(lease);
}

#[tokio::test(flavor = "multi_thread")]
async fn early_relay_declines_unpublished_writer_context() {
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
            .early_relay(block.clone(), true, true)
            .await
            .unwrap()
            .is_some());
        // Model the interval between an authoritative mutation and its snapshot publication.
        let update = read.early_relay.updating_context();
        assert!(read
            .clone()
            .early_relay(block.clone(), true, true)
            .await
            .unwrap()
            .is_none());
        drop(update);
        assert!(read
            .clone()
            .early_relay(block, true, true)
            .await
            .unwrap()
            .is_some());
    })
    .await
    .expect("writer-context admission must finish");
}

/// The response can wake a replay before `send` returns on the writer thread.
#[test]
fn early_relay_rejected_commit_releases_context_before_response() {
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Wake, Waker},
    };

    use crate::{
        arbitrary::Prepare,
        service::write::{BlockWriteSender, NonFinalizedWriteMessage},
        tests::setup::new_state_with_mainnet_genesis,
    };

    struct ContextAtWake {
        state: Arc<EarlyRelayState>,
        version: std::sync::mpsc::Sender<u64>,
    }

    impl Wake for ContextAtWake {
        fn wake(self: Arc<Self>) {
            self.version
                .send(self.state.context_version.load(Ordering::SeqCst))
                .unwrap();
        }
    }

    let _init_guard = zebra_test::init();
    let (finalized, non_finalized, genesis) = new_state_with_mainnet_genesis();
    let (tip_sender, _, _) = ChainTipSender::new(None, &Network::Mainnet);
    let (state_sender, _snapshot) = watch::channel(non_finalized.clone());
    let relay = EarlyRelayState::new(None);
    let (sender, _resets, _rejections, worker) = BlockWriteSender::spawn(
        finalized,
        non_finalized,
        tip_sender,
        state_sender,
        false,
        None,
        relay.clone(),
        None,
    );
    let (version_tx, version_rx) = std::sync::mpsc::channel();
    let waker = Waker::from(Arc::new(ContextAtWake {
        state: relay,
        version: version_tx,
    }));
    let (rsp_tx, mut rsp_rx) = tokio::sync::oneshot::channel();
    assert!(Pin::new(&mut rsp_rx)
        .poll(&mut Context::from_waker(&waker))
        .is_pending());
    // Genesis is already finalized, so this commit must be rejected.
    sender
        .non_finalized
        .as_ref()
        .unwrap()
        .send(NonFinalizedWriteMessage::Commit(
            (genesis.block.clone().prepare(), rsp_tx, None, None),
            std::time::Instant::now(),
        ))
        .unwrap();
    let version = version_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(rsp_rx.try_recv().unwrap().is_err());
    drop(sender);
    Arc::try_unwrap(worker.unwrap()).unwrap().join().unwrap();
    assert_eq!(
        version % 2,
        0,
        "rejection must not wake an unauthenticated replay"
    );
}

/// Both completion channels must observe a published checkpoint tip and usable authentication.
#[test]
fn early_relay_checkpoint_commit_publishes_context_before_notifications() {
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
        state: Arc<EarlyRelayState>,
        observed: std::sync::mpsc::Sender<(u64, Option<block::Hash>)>,
    }

    impl Wake for PublishedContextAtWake {
        fn wake(self: Arc<Self>) {
            self.observed
                .send((
                    self.state.context_version.load(Ordering::SeqCst),
                    self.state.tip.as_ref().and_then(ChainTip::best_tip_hash),
                ))
                .unwrap();
        }
    }

    let _init_guard = zebra_test::init();
    let (finalized, non_finalized, genesis) = new_state_with_mainnet_genesis();
    let (tip_sender, tip, _) =
        ChainTipSender::new(Some(ChainTipBlock::from(genesis)), &Network::Mainnet);
    let (state_sender, _snapshot) = watch::channel(non_finalized.clone());
    let relay = EarlyRelayState::new(Some(tip));
    let (sender, _resets, _rejections, worker) = BlockWriteSender::spawn(
        finalized,
        non_finalized,
        tip_sender,
        state_sender,
        true,
        None,
        relay.clone(),
        None,
    );
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    let waker = Waker::from(Arc::new(PublishedContextAtWake {
        state: relay,
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
    for (version, tip) in observations {
        assert_eq!(
            version % 2,
            0,
            "checkpoint completion must allow authentication"
        );
        assert_eq!(
            tip,
            Some(hash),
            "checkpoint completion must publish the parent"
        );
    }
}
