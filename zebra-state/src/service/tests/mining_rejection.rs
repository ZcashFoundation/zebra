//! Authenticated rejection must invalidate queued and already-published speculative mining work.

use std::{sync::Arc, time::Duration};

use tokio::sync::{oneshot, watch};
use tower::{Service, ServiceExt};
use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Block, Height},
    parameters::{testnet::ConfiguredActivationHeights, Network, NetworkUpgrade},
    transaction::{LockTime, Transaction},
    transparent,
    work::equihash,
};

use crate::{
    service::{non_finalized_state::MAX_EARLY_RELAY_REJECTIONS, StateService},
    CheckpointVerifiedBlock, Config, MiningCandidateChanges, ReadRequest, ReadResponse,
    ReadStateService, Request, Response,
};

async fn state(finalized_only: bool) -> (StateService, ReadStateService) {
    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(1),
            ..Default::default()
        }
        .into(),
    );
    let (mut state, read, _, _) =
        StateService::new(Config::ephemeral(), &network, Height(0), 0).await;
    state
        .ready()
        .await
        .unwrap()
        .call(Request::CommitCheckpointVerifiedBlock(
            block::genesis::regtest_genesis_block().into(),
        ))
        .await
        .unwrap();
    if !finalized_only {
        let parent = candidate(&read, false, 0).await;
        state
            .ready()
            .await
            .unwrap()
            .call(Request::CommitSemanticallyVerifiedBlock(
                CheckpointVerifiedBlock::from(parent).into(),
            ))
            .await
            .unwrap();
    }
    (state, read)
}

/// Uses real Merkle and NU5 authorizing-data commitments, with an optional missing-input failure.
///
/// Higher ranks select later satisfying nonces, giving siblings that differ only by nonce.
async fn candidate(read: &ReadStateService, missing_input: bool, rank: usize) -> Arc<Block> {
    let ReadResponse::ChainInfo(info) = read.clone().oneshot(ReadRequest::ChainInfo).await.unwrap()
    else {
        panic!("state returns template context");
    };
    let height = info.tip_height.next().unwrap();
    let mut transactions = vec![Arc::new(Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![transparent::Input::Coinbase {
            height,
            data: vec![0],
            sequence: u32::MAX,
        }],
        vec![transparent::Output::new(
            zebra_chain::parameters::subsidy::block_subsidy(height, &read.network).unwrap(),
            transparent::Script::new(&[0x51]),
        )],
        LockTime::unlocked(),
        height,
    ))];
    if missing_input {
        transactions.push(Arc::new(Transaction::test_v5(
            NetworkUpgrade::Nu5,
            vec![transparent::Input::PrevOut {
                outpoint: transparent::OutPoint {
                    hash: [8; 32].into(),
                    index: 0,
                },
                unlock_script: transparent::Script::new(&[0x51]),
                sequence: u32::MAX,
            }],
            vec![transparent::Output::new(
                Amount::<NonNegative>::try_from(1).unwrap(),
                transparent::Script::new(&[0x51]),
            )],
            LockTime::unlocked(),
            height,
        )));
    }
    let mut block = Block {
        header: Arc::new(block::Header {
            version: 4,
            previous_block_hash: info.tip_hash,
            merkle_root: transactions.iter().map(|tx| tx.hash()).collect(),
            commitment_bytes: [0; 32].into(),
            time: info.cur_time.into(),
            difficulty_threshold: info.expected_difficulty,
            nonce: [0; 32].into(),
            solution: equihash::Solution::Regtest([0; 36]),
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
    let target = info.expected_difficulty.to_expanded().unwrap();
    // Regtest disables Equihash, but the authenticated fixture still satisfies its PoW target.
    (0u32..100_000)
        .filter(|nonce| {
            let mut bytes = [0; 32];
            bytes[..4].copy_from_slice(&nonce.to_le_bytes());
            Arc::make_mut(&mut block.header).nonce = bytes.into();
            block.hash() <= target
        })
        .nth(rank)
        .expect("the regtest target is reached within the bounded nonce search");
    Arc::new(block)
}

async fn subscribe(read: &ReadStateService) -> MiningCandidateChanges {
    let ReadResponse::MiningCandidateChanges(changes) = read
        .clone()
        .oneshot(ReadRequest::MiningCandidateChanges)
        .await
        .unwrap()
    else {
        panic!("state returns a persistent mining subscription");
    };
    changes
}

#[test]
fn rejection_during_queued_mining_computation_prevents_publication() {
    let _init_guard = zebra_test::init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), async {
            let (mut state, read) = state(false).await;
            let block = candidate(&read, true, 0).await;
            assert!(
                crate::service::read::difficulty::mining_candidate_chain_info(
                    &read.latest_non_finalized_state(),
                    &read.db,
                    &read.network,
                    block.clone(),
                )
                .is_some(),
                "the authenticated body must produce work before rejection"
            );
            let Response::EarlyRelay(Some(authenticated)) = state
                .ready()
                .await
                .unwrap()
                .call(Request::AuthenticateEarlyRelay(block.clone()))
                .await
                .unwrap()
            else {
                panic!("the NU5 body must authenticate against the non-finalized parent");
            };
            let mut changes = subscribe(&read).await;
            let (release, released) = oneshot::channel::<()>();
            let (started, running) = oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                // Dropping release on a failed assertion or timeout also unblocks shutdown.
                let _ = released.blocking_recv();
            });
            running.await.unwrap();
            let Response::MiningCandidate(Some(guard)) = state
                .ready()
                .await
                .unwrap()
                .call(Request::MiningCandidate(block.clone()))
                .await
                .unwrap()
            else {
                panic!("the first candidate reserves the computation slot");
            };
            // The queued blocking computation retains a clone of the publication's slot permit.
            while Arc::strong_count(&guard.0.borrow()) < 2 {
                tokio::task::yield_now().await;
            }
            // The sole async worker must finish polling publication before running this task,
            // proving its blocking computation is queued, not just about to be spawned.
            tokio::spawn(async {}).await.unwrap();
            assert!(!changes.0.has_changed().unwrap());
            authenticated.reject();
            assert!(authenticated.is_rejected());
            release.send(()).unwrap();
            blocker.await.unwrap();
            tokio::select! {
                biased;
                _ = changes.changed() => panic!("a rejected queued candidate must never publish"),
                _ = guard.0.closed() => {}
            }
            assert!(!changes.is_current(block.hash()));
            assert!(changes.block(block.hash()).is_none());
            assert_eq!(
                read.oneshot(ReadRequest::MiningCandidate).await.unwrap(),
                ReadResponse::MiningCandidate(None)
            );
            drop(guard);
            // Receiver closure is signalled before the task drops its last reference to the slot.
            drop(
                state
                    .mining_candidate_slot
                    .clone()
                    .acquire_owned()
                    .await
                    .unwrap(),
            );
            assert!(matches!(
                state
                    .oneshot(Request::MiningCandidate(block))
                    .await
                    .unwrap(),
                Response::MiningCandidate(None)
            ));
            assert!(
                !changes.0.has_changed().unwrap(),
                "replay must not publish again"
            );
        })
        .await
        .expect("queued mining rejection finishes without waiting for candidate expiry");
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn published_mining_rejection_invalidates_live_and_detached_reads() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut state, read) = state(true).await;
        assert!(read.latest_non_finalized_state().best_chain().is_none());
        let block = candidate(&read, true, 0).await;
        let hash = block.hash();
        let mut changes = subscribe(&read).await;
        let Response::MiningCandidate(Some(guard)) = state
            .ready()
            .await
            .unwrap()
            .call(Request::MiningCandidate(block.clone()))
            .await
            .unwrap()
        else {
            panic!("finalized-only parent context supports speculative mining");
        };
        changes.changed().await;
        assert!(changes.is_current(hash));
        assert!(Arc::ptr_eq(&changes.block(hash).unwrap(), &block));
        assert!(matches!(
            read.clone().oneshot(ReadRequest::MiningCandidate).await.unwrap(),
            ReadResponse::MiningCandidate(Some(info)) if info.tip_hash == hash
        ));
        // Freeze the publication independently of the task which normally withdraws it.
        let (publication, receiver) = watch::channel(changes.0.borrow().clone());
        let mut detached = changes.clone();
        detached.0 = receiver;
        assert_eq!(
            state
                .ready()
                .await
                .unwrap()
                .call(Request::RejectBlock(block.clone()))
                .await
                .unwrap(),
            Response::Rejected(true),
            "rejection authentication must work before a non-finalized snapshot exists"
        );
        assert!(detached.0.borrow().is_some());
        assert!(
            !detached.is_current(hash),
            "cached template work is no longer authorized"
        );
        assert!(
            detached.block(hash).is_none(),
            "transaction selection cannot use the rejected body"
        );
        assert!(!changes.is_current(hash));
        assert!(changes.block(hash).is_none());
        assert_eq!(
            read.oneshot(ReadRequest::MiningCandidate).await.unwrap(),
            ReadResponse::MiningCandidate(None)
        );
        drop((detached, publication, guard));
        // Wait for cancellation to free the slot, so a busy slot cannot mask replay admission.
        drop(
            state
                .mining_candidate_slot
                .clone()
                .acquire_owned()
                .await
                .unwrap(),
        );
        changes.mark_seen();
        assert!(matches!(
            state
                .oneshot(Request::MiningCandidate(block))
                .await
                .unwrap(),
            Response::MiningCandidate(None)
        ));
        assert!(
            !changes.0.has_changed().unwrap(),
            "replay must not publish again"
        );
    })
    .await
    .expect("published rejection is visible without waiting for candidate expiry");
}

#[tokio::test(flavor = "multi_thread")]
async fn rejection_capacity_disables_mining_without_rejecting_an_unknown_block() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut state, read) = state(false).await;
        let block = candidate(&read, false, 0).await;
        let hash = block.hash();
        let Response::EarlyRelay(Some(authenticated)) = state
            .ready()
            .await
            .unwrap()
            .call(Request::AuthenticateEarlyRelay(block.clone()))
            .await
            .unwrap()
        else {
            panic!("the ordinary candidate has an authenticated body");
        };
        let mut changes = subscribe(&read).await;
        let Response::MiningCandidate(Some(guard)) = state
            .ready()
            .await
            .unwrap()
            .call(Request::MiningCandidate(block.clone()))
            .await
            .unwrap()
        else {
            panic!("mining is enabled before rejection capacity fills");
        };
        changes.changed().await;
        assert!(changes.is_current(hash));
        let (publication, receiver) = watch::channel(changes.0.borrow().clone());
        let mut detached = changes.clone();
        detached.0 = receiver;
        {
            // Only the capacity setup uses synthetic hashes; the live candidate is authenticated.
            let rejected = read.latest_non_finalized_state().rejected_blocks.clone();
            let mut rejected = rejected.lock().unwrap();
            for index in 0..MAX_EARLY_RELAY_REJECTIONS {
                let mut other = block::Hash([0; 32]);
                other.0[..4].copy_from_slice(&u32::try_from(index).unwrap().to_le_bytes());
                assert_ne!(other, hash);
                rejected.reject(other, block.coinbase_height().unwrap());
            }
            assert!(rejected.is_full());
            assert!(!rejected.contains(hash));
        }
        assert!(
            !authenticated.is_rejected(),
            "capacity is not a deterministic validation failure"
        );
        assert!(!changes.is_current(hash));
        assert!(!detached.is_current(hash));
        assert!(detached.block(hash).is_none());
        assert_eq!(
            read.clone()
                .oneshot(ReadRequest::MiningCandidate)
                .await
                .unwrap(),
            ReadResponse::MiningCandidate(None)
        );
        drop((detached, publication, guard));
        drop(
            state
                .mining_candidate_slot
                .clone()
                .acquire_owned()
                .await
                .unwrap(),
        );
        changes.mark_seen();
        assert!(matches!(
            state
                .ready()
                .await
                .unwrap()
                .call(Request::MiningCandidate(block.clone()))
                .await
                .unwrap(),
            Response::MiningCandidate(None)
        ));
        assert!(!changes.0.has_changed().unwrap());
        assert_eq!(
            state
                .oneshot(Request::CommitSemanticallyVerifiedBlock(
                    CheckpointVerifiedBlock::from(block).into(),
                ))
                .await
                .unwrap(),
            Response::Committed(hash),
            "ordinary full validation must remain available to advance the chain"
        );
    })
    .await
    .expect("rejection capacity disables only speculative work");
}

/// Required authentication accepts a side-fork parent, so its rejection suppresses replays.
#[tokio::test(flavor = "multi_thread")]
async fn side_fork_child_authenticates_and_remembers_rejection() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut state, read) = state(false).await;
        let first = candidate(&read, false, 0).await;
        let second = candidate(&read, false, 1).await;
        assert_ne!(first.hash(), second.hash());
        let received = std::time::Instant::now();
        let commit = |block, received_time| {
            let mut block: crate::SemanticallyVerifiedBlock =
                CheckpointVerifiedBlock::from(block).into();
            block.received_time = Some(received_time);
            Request::CommitSemanticallyVerifiedBlock(block)
        };
        state
            .ready()
            .await
            .unwrap()
            .call(commit(first, received + Duration::from_secs(1)))
            .await
            .unwrap();
        let child = candidate(&read, false, 0).await;
        state
            .ready()
            .await
            .unwrap()
            .call(commit(second.clone(), received))
            .await
            .unwrap();
        assert_eq!(read.best_tip().map(|(_, hash)| hash), Some(second.hash()));
        let Response::EarlyRelay(Some(guard)) = state
            .ready()
            .await
            .unwrap()
            .call(Request::AuthenticateEarlyRelay(child.clone()))
            .await
            .unwrap()
        else {
            panic!("a parent retained on a side fork authenticates its child");
        };
        guard.reject();
        let Response::EarlyRelay(Some(replay)) = state
            .ready()
            .await
            .unwrap()
            .call(Request::EarlyRelay {
                block: child,
                require_authentication: true,
            })
            .await
            .unwrap()
        else {
            panic!("the replayed side-fork body authenticates again");
        };
        assert!(
            replay.is_rejected(),
            "the rejection skips replayed verification"
        );
    })
    .await
    .expect("side-fork authentication finishes without retries");
}
