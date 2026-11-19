//! Failed candidate computations release admission without waiting for the verifier's guard.

use std::{sync::Arc, time::Duration};

use tokio::sync::oneshot;
use tower::{Service, ServiceExt};
use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Block, Height},
    parameters::{
        subsidy::scheduled_block_subsidy, testnet::ConfiguredActivationHeights, Network,
        NetworkUpgrade,
    },
    transaction::{LockTime, Transaction},
    transparent,
    work::{difficulty::ParameterDifficulty as _, equihash},
};

use crate::{
    service::{arbitrary::TEST_MINING_CANDIDATE_LIFETIME, StateService},
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
    state.mining_candidate_lifetime = TEST_MINING_CANDIDATE_LIFETIME;
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
            scheduled_block_subsidy(height, &read.network).unwrap(),
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

/// Returns whether `hash` is the published candidate.
async fn is_published(read: &ReadStateService, hash: block::Hash) -> bool {
    matches!(
        read.clone().oneshot(ReadRequest::MiningCandidate).await.unwrap(),
        ReadResponse::MiningCandidate(Some(info)) if info.tip_hash == hash
    )
}

async fn assert_unpublished_candidate_releases_slot(
    state: &mut StateService,
    read: &ReadStateService,
    rejected: Arc<Block>,
    next: Arc<Block>,
) {
    let mut changes = subscribe(read).await;
    let Response::MiningCandidate(Some(mut guard)) = state
        .ready()
        .await
        .unwrap()
        .call(Request::MiningCandidate(rejected))
        .await
        .unwrap()
    else {
        panic!("the failed candidate initially reserves the computation slot");
    };
    guard
        .0
        .wait_for(Option::is_none)
        .await
        .expect("a completed computation without context releases its admission");
    assert!(!changes.0.has_changed().unwrap());
    assert_eq!(
        read.clone()
            .oneshot(ReadRequest::MiningCandidate)
            .await
            .unwrap(),
        ReadResponse::MiningCandidate(None)
    );

    let Response::MiningCandidate(Some(next_guard)) = state
        .ready()
        .await
        .unwrap()
        .call(Request::MiningCandidate(next.clone()))
        .await
        .unwrap()
    else {
        panic!("the next candidate is admitted while the failed candidate's guard is retained");
    };
    changes.changed().await;
    assert!(is_published(read, next.hash()).await);
    assert!(guard.0.borrow().is_none());
    drop((guard, next_guard));
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_mining_candidate_context_releases_slot_while_guard_is_retained() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut state, read) = state(false).await;
        let next = candidate(&read, false, 0).await;
        // A child of the best tip whose proof of work only meets its own declared target.
        let mut rejected = (*next).clone();
        Arc::make_mut(&mut rejected.header).difficulty_threshold =
            Network::Mainnet.target_difficulty_limit().to_compact();
        assert_ne!(
            rejected.header.difficulty_threshold,
            next.header.difficulty_threshold
        );
        assert_eq!(
            rejected.header.previous_block_hash,
            read.best_tip().unwrap().1
        );
        assert_unpublished_candidate_releases_slot(&mut state, &read, Arc::new(rejected), next)
            .await;
    })
    .await
    .expect("a rejected context must not reserve admission until its verifier finishes");
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_mining_candidate_body_releases_slot_while_guard_is_retained() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut state, read) = state(false).await;
        let next = candidate(&read, false, 0).await;
        let original = candidate(&read, true, 0).await;
        let mut invalid = (*original).clone();
        let transaction = &original.transactions[1];
        let mut inputs = transaction.inputs();
        let transparent::Input::PrevOut { unlock_script, .. } = &mut inputs[0] else {
            panic!("the fixture's non-coinbase transaction spends a transparent input");
        };
        *unlock_script = transparent::Script::new(&[0x52]);
        invalid.transactions[1] = Arc::new((**transaction).clone().with_transparent_inputs(inputs));
        assert_eq!(invalid.hash(), original.hash());
        assert_eq!(invalid.transactions[1].hash(), transaction.hash());
        assert_ne!(invalid.auth_data_root(), original.auth_data_root());
        assert_unpublished_candidate_releases_slot(&mut state, &read, Arc::new(invalid), next)
            .await;
    })
    .await
    .expect("an invalid body must not reserve admission until its verifier finishes");
}

#[test]
fn timed_out_mining_candidate_keeps_queued_computation_bounded() {
    let _init_guard = zebra_test::init();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(60), async {
            // A checkpointed parent avoids `block_in_place` commits, so this test can pause time.
            let (mut state, read) = state(true).await;
            state.mining_candidate_lifetime = Duration::from_secs(30);
            let block = candidate(&read, false, 0).await;
            let mut changes = subscribe(&read).await;
            let (release, released) = oneshot::channel::<()>();
            let (started, running) = oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                // Dropping release after a failed assertion also unblocks runtime shutdown.
                let _ = released.blocking_recv();
            });
            running.await.unwrap();
            let Response::MiningCandidate(Some(mut guard)) = state
                .ready()
                .await
                .unwrap()
                .call(Request::MiningCandidate(block.clone()))
                .await
                .unwrap()
            else {
                panic!("the first candidate reserves the computation slot");
            };
            let permit = Arc::downgrade(guard.0.borrow().as_ref().unwrap());
            while permit.strong_count() < 2 {
                tokio::task::yield_now().await;
            }
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(31)).await;
            guard
                .0
                .wait_for(Option::is_none)
                .await
                .expect("unpublished work releases its guard's reservation at its deadline");
            tokio::time::resume();
            assert_eq!(permit.strong_count(), 1);
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

            release.send(()).unwrap();
            blocker.await.unwrap();
            // The sole blocking thread runs the expired computation before this task. The read
            // below is answered from memory, so it doesn't wait for that thread.
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            assert_eq!(
                read.clone()
                    .oneshot(ReadRequest::MiningCandidate)
                    .await
                    .unwrap(),
                ReadResponse::MiningCandidate(None)
            );
            assert_eq!(permit.strong_count(), 0);
            let Response::MiningCandidate(Some(next_guard)) = state
                .ready()
                .await
                .unwrap()
                .call(Request::MiningCandidate(block.clone()))
                .await
                .unwrap()
            else {
                panic!("completed expired work must not reserve the next candidate's slot");
            };
            changes.changed().await;
            assert!(is_published(&read, block.hash()).await);
            drop((guard, next_guard));
        })
        .await
        .expect("candidate timeout must bound queued work without retaining failed admission");
    });
}

/// A candidate's computation must be queued when the candidate is requested, not when the task
/// that publishes its result first runs.
#[test]
fn candidate_computation_is_queued_by_the_request() {
    let _init_guard = zebra_test::init();

    // The state is created on its own runtime: committing its blocks needs blocking threads,
    // and a multi-threaded runtime.
    let setup = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (mut state, read, block) = setup.block_on(async {
        let (state, read) = state(false).await;
        let block = candidate(&read, false, 0).await;
        (state, read, block)
    });

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
        .block_on(async {
            // Keep the only blocking thread busy, so a queued computation stays queued.
            let (started, started_rx) = oneshot::channel();
            let (release, held) = std::sync::mpsc::channel::<()>();
            let busy = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                let _ = held.recv_timeout(Duration::from_secs(30));
            });
            started_rx.await.unwrap();

            let _changes = subscribe(&read).await;
            let Response::MiningCandidate(Some(guard)) = state
                .ready()
                .await
                .unwrap()
                .call(Request::MiningCandidate(block))
                .await
                .unwrap()
            else {
                panic!("a child of the best tip is admitted");
            };

            // The computation holds its own reference to the candidate's slot until it ends.
            let slot_references = || guard.0.borrow().as_ref().map(Arc::strong_count);
            assert_eq!(slot_references(), Some(2));

            // This task doesn't let any other task run until the computation has ended, so only
            // the request can have queued it.
            drop(release);
            let waiting_since = std::time::Instant::now();
            while slot_references() != Some(1) {
                assert!(
                    waiting_since.elapsed() < Duration::from_secs(10),
                    "the computation was not queued by the request",
                );
                std::thread::sleep(Duration::from_millis(1));
            }

            busy.await.unwrap();
        });
    drop(setup);
}
