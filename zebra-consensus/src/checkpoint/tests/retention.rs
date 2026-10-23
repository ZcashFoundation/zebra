//! Retained-body regressions through the real checkpoint service entry point.
//!
//! Synthetic blocks and trusted checkpoint lists isolate admission and ownership, not network
//! consensus validity. The progress test commits unchanged historical blocks to the real state.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Weak,
};

use zebra_chain::{parameters::testnet::Parameters, serialization::ZcashSerialize, transparent};
use zebra_node_services::constants::DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT;
use zebra_test::mock_service::MockService;

use super::*;

// The unchanged syncer admits default batches of 1,000 requests, starting another batch below
// that threshold. Its ordinary overlapping window can therefore contain 1,999 requests.
const RETAINED_BLOCK_LIMIT: usize = 2 * DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT;

fn network_without_pow() -> Network {
    Parameters::build()
        .with_disable_pow(true)
        .to_network()
        .unwrap()
}

fn genesis() -> Arc<Block> {
    Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..]).unwrap()
}

fn block_at(height: usize, parent: block::Hash) -> Arc<Block> {
    let mut block =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..]).unwrap();
    let mut inputs = block.transactions[0].inputs();
    let transparent::Input::Coinbase {
        height: coinbase_height,
        ..
    } = &mut inputs[0]
    else {
        panic!("the fixture has a coinbase input")
    };
    *coinbase_height = block::Height(height.try_into().unwrap());
    block.transactions[0] = Arc::new(
        (*block.transactions[0])
            .clone()
            .with_transparent_inputs(inputs),
    );
    let merkle_root = block.transactions.iter().map(|tx| tx.hash()).collect();
    let header = Arc::make_mut(&mut block.header);
    header.previous_block_hash = parent;
    header.merkle_root = merkle_root;
    Arc::new(block)
}

fn blocks_through(last_height: usize) -> Vec<Arc<Block>> {
    let mut blocks = vec![genesis()];
    for height in 1..=last_height {
        blocks.push(block_at(height, blocks.last().unwrap().hash()));
    }
    blocks
}

fn sparse_checkpoints() -> Vec<(block::Height, block::Hash)> {
    let mut checkpoints = vec![(block::Height(0), genesis().hash())];
    let last_height = MAX_CHECKPOINT_HEIGHT_GAP + 2 * RETAINED_BLOCK_LIMIT;
    for height in (MAX_CHECKPOINT_HEIGHT_GAP..=last_height).step_by(MAX_CHECKPOINT_HEIGHT_GAP) {
        let height = u32::try_from(height).unwrap();
        let mut hash = [0; 32];
        hash[..4].copy_from_slice(&height.to_le_bytes());
        checkpoints.push((block::Height(height), block::Hash(hash)));
    }
    checkpoints
}

fn retained_bodies(blocks: &[Weak<Block>]) -> usize {
    blocks
        .iter()
        .filter(|block| block.strong_count() != 0)
        .count()
}

async fn assert_bodies_released(blocks: &[Weak<Block>]) {
    timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        while retained_bodies(blocks) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("finished owners must release their block bodies");
}

#[tokio::test]
async fn sparse_receiver_drops_bound_retained_checkpoint_bodies() {
    let _init_guard = zebra_test::init();
    let state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut verifier =
        CheckpointVerifier::from_list(sparse_checkpoints(), &network_without_pow(), None, state)
            .unwrap();
    let mut bodies = Vec::new();

    for index in 0..=RETAINED_BLOCK_LIMIT {
        let block = block_at(MAX_CHECKPOINT_HEIGHT_GAP + 2 * index, block::Hash([7; 32]));
        bodies.push(Arc::downgrade(&block));
        if let Some(result) = verifier.call(block).now_or_never() {
            let error = result.expect_err("a sparse chain cannot commit");
            assert_eq!(
                error.misbehavior_score(),
                0,
                "pressure is not peer misconduct"
            );
        }
    }
    tokio::task::yield_now().await;

    assert_eq!(verifier.previous_checkpoint_height(), BeforeGenesis);
    let retained = retained_bodies(&bodies);
    let queued_heights = verifier.queued.len();
    assert!(verifier.queued.values().all(|blocks| !blocks.is_empty()));
    drop(verifier);
    assert_bodies_released(&bodies).await;
    assert!(
        retained <= RETAINED_BLOCK_LIMIT,
        "dropped sparse responses retained {retained} bodies, exceeding {RETAINED_BLOCK_LIMIT}",
    );
    assert!(
        queued_heights <= RETAINED_BLOCK_LIMIT,
        "rejected admissions must not leave unbounded height indexes",
    );
}

#[tokio::test]
async fn duplicate_replacements_bound_retained_checkpoint_bodies() {
    let _init_guard = zebra_test::init();
    let state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut verifier =
        CheckpointVerifier::from_list(sparse_checkpoints(), &network_without_pow(), None, state)
            .unwrap();
    let original = block_at(MAX_CHECKPOINT_HEIGHT_GAP, block::Hash([7; 32]));
    let mut bodies = Vec::new();
    let runtime = tokio::runtime::Handle::current().metrics();
    let initial_tasks = runtime.num_alive_tasks();

    // Do not yield: replacing a queue entry must not leave its body in an unbounded set of
    // detached waiters that only disappear when the runtime eventually polls them.
    for _ in 0..=RETAINED_BLOCK_LIMIT {
        let block = Arc::new((*original).clone());
        bodies.push(Arc::downgrade(&block));
        if let Some(result) = verifier.call(block).now_or_never() {
            let error = result.expect_err("a sparse chain cannot commit");
            assert_eq!(
                error.misbehavior_score(),
                0,
                "pressure is not peer misconduct"
            );
        }
    }
    let retained = retained_bodies(&bodies);
    let retained_tasks = runtime.num_alive_tasks().saturating_sub(initial_tasks);
    drop(verifier);
    assert_bodies_released(&bodies).await;
    assert!(
        retained <= RETAINED_BLOCK_LIMIT,
        "duplicate replacement retained {retained} distinct bodies behind one height",
    );
    assert!(
        retained_tasks <= RETAINED_BLOCK_LIMIT,
        "duplicate replacement retained {retained_tasks} detached tasks",
    );
    assert!(
        runtime.num_alive_tasks() <= initial_tasks,
        "discarded duplicate owners must finish when their bodies are released",
    );
}

#[tokio::test]
async fn verified_checkpoint_bodies_stay_bounded_while_state_waits() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        let blocks = blocks_through(RETAINED_BLOCK_LIMIT);
        let checkpoints: Vec<_> = blocks
            .iter()
            .map(|block| (block.coinbase_height().unwrap(), block.hash()))
            .collect();
        let (release, released) = tokio::sync::watch::channel(false);
        let (started, mut starts) = tokio::sync::mpsc::channel(1);
        let (committed, mut commits) = tokio::sync::mpsc::channel(RETAINED_BLOCK_LIMIT + 1);
        let state = tower::service_fn(move |request| {
            let mut released = released.clone();
            let started = started.clone();
            let committed = committed.clone();
            async move {
                let zs::Request::CommitCheckpointVerifiedBlock(block) = request else {
                    panic!("only commits are expected while the state is gated")
                };
                let _ = started.try_send(block.hash);
                released.wait_for(|released| *released).await.unwrap();
                committed.send(block.hash).await.unwrap();
                Ok::<_, BoxError>(zs::Response::Committed(block.hash))
            }
        });
        let mut verifier =
            CheckpointVerifier::from_list(checkpoints, &network_without_pow(), None, state)
                .unwrap();
        let mut bodies = Vec::new();
        let mut accepted = std::collections::HashSet::new();
        for block in blocks {
            let hash = block.hash();
            bodies.push(Arc::downgrade(&block));
            match verifier.call(block).now_or_never() {
                None => assert!(accepted.insert(hash)),
                Some(Err(error)) => assert_eq!(error.misbehavior_score(), 0),
                Some(Ok(_)) => panic!("the state has not been released"),
            }
        }
        starts.recv().await.unwrap();
        let retained = retained_bodies(&bodies);
        assert!(
            retained != 0,
            "accepted commits retain bodies while the state waits"
        );
        let retained_work = verifier.retained_blocks.clone();
        assert_eq!(retained_work.load(Ordering::Relaxed), RETAINED_BLOCK_LIMIT);
        verifier.reset_progress(None);
        assert!(matches!(
            verifier.call(genesis()).now_or_never(),
            Some(Err(VerifyCheckpointError::QueuedLimit))
        ));
        assert_eq!(
            retained_work.load(Ordering::Relaxed),
            RETAINED_BLOCK_LIMIT,
            "reset cannot free reservations held by accepted state-waiting owners",
        );

        release.send(true).unwrap();
        while !accepted.is_empty() {
            assert!(accepted.remove(&commits.recv().await.unwrap()));
        }
        drop(verifier);
        assert_bodies_released(&bodies).await;
        assert_eq!(retained_work.load(Ordering::Relaxed), 0);
        assert!(
            retained <= RETAINED_BLOCK_LIMIT,
            "verified/state-waiting owners retained {retained} bodies after leaving the queue",
        );
    })
    .await
    .expect("accepted commits must finish after the state is released");
}

#[tokio::test]
async fn default_prefetch_survives_overlapping_checkpoint_commits() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        let blocks = blocks_through(RETAINED_BLOCK_LIMIT);
        let bodies: Vec<_> = blocks.iter().map(Arc::downgrade).collect();
        let checkpoints: Vec<_> = blocks
            .iter()
            .step_by(MAX_CHECKPOINT_HEIGHT_GAP)
            .map(|block| (block.coinbase_height().unwrap(), block.hash()))
            .collect();
        let initial_tip = Some((block::Height(0), blocks[0].hash()));
        let (release, released) = tokio::sync::watch::channel(false);
        let (started, mut starts) = tokio::sync::mpsc::channel(1);
        let state = tower::service_fn(move |request| {
            let mut released = released.clone();
            let started = started.clone();
            async move {
                let zs::Request::CommitCheckpointVerifiedBlock(block) = request else {
                    panic!("only checkpoint commits are expected")
                };
                // One completion lets the normal syncer submit its next entire default batch.
                if block.height != block::Height(1) {
                    let _ = started.try_send(block.height);
                    released.wait_for(|released| *released).await.unwrap();
                }
                Ok::<_, BoxError>(zs::Response::Committed(block.hash))
            }
        });
        let mut verifier =
            CheckpointVerifier::from_list(checkpoints, &network_without_pow(), initial_tip, state)
                .unwrap();
        let mut blocks = blocks.into_iter().skip(1);
        let mut responses = std::collections::VecDeque::with_capacity(RETAINED_BLOCK_LIMIT);
        for block in blocks.by_ref().take(DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT) {
            let hash = block.hash();
            responses.push_back((hash, verifier.ready().await.unwrap().call(block)));
        }
        let (first_hash, first_response) = responses.pop_front().unwrap();
        assert_eq!(first_response.await.unwrap(), first_hash);
        assert_eq!(responses.len(), DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT - 1);
        starts.recv().await.unwrap();
        assert!(
            !verifier.queued.is_empty(),
            "the default batch includes a partial future checkpoint range",
        );

        // A whole second batch arrives while earlier ranges are still committing. None of
        // these ordinary prefetched blocks may be rejected merely for crossing a checkpoint.
        for block in blocks {
            let hash = block.hash();
            responses.push_back((hash, verifier.ready().await.unwrap().call(block)));
        }
        assert_eq!(
            retained_bodies(&bodies),
            RETAINED_BLOCK_LIMIT - 1,
            "the 1,999 accepted requests remain owned while state commits are blocked",
        );

        release.send(true).unwrap();
        while let Some((hash, response)) = responses.pop_front() {
            assert_eq!(
                response.await.unwrap(),
                hash,
                "normal prefetch must not need resubmission",
            );
        }
        drop(verifier);
        assert_bodies_released(&bodies).await;
    })
    .await
    .expect("ordinary overlapping prefetch must complete without retrying a checkpoint range");
}

#[tokio::test]
async fn trusted_custom_gap_larger_than_default_budget_completes() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        let blocks = blocks_through(RETAINED_BLOCK_LIMIT + 1);
        let last = blocks.last().unwrap();
        let initial_tip = (block::Height(0), blocks[0].hash());
        let checkpoints = [initial_tip, (last.coinbase_height().unwrap(), last.hash())];
        let state = tower::service_fn(|request| async move {
            let zs::Request::CommitCheckpointVerifiedBlock(block) = request else {
                panic!("only checkpoint commits are expected")
            };
            Ok::<_, BoxError>(zs::Response::Committed(block.hash))
        });
        let mut verifier = CheckpointVerifier::from_list(
            checkpoints,
            &network_without_pow(),
            Some(initial_tip),
            state,
        )
        .unwrap();
        let mut responses = FuturesUnordered::new();
        for block in blocks.into_iter().skip(1).rev() {
            let hash = block.hash();
            responses.push(
                verifier
                    .ready()
                    .await
                    .unwrap()
                    .call(block)
                    .map(move |result| (hash, result)),
            );
        }
        while let Some((hash, response)) = responses.next().await {
            assert_eq!(response.unwrap(), hash);
        }
        assert_eq!(verifier.previous_checkpoint_height(), FinalCheckpoint);
    })
    .await
    .expect("trusted custom checkpoint gaps must not be capped by generator defaults");
}

#[tokio::test]
async fn oversized_internal_checkpoint_body_is_not_retained() {
    let _init_guard = zebra_test::init();
    let mut block = block_at(MAX_CHECKPOINT_HEIGHT_GAP, block::Hash([7; 32]));
    let block_mut = Arc::make_mut(&mut block);
    let coinbase = Arc::make_mut(&mut block_mut.transactions[0]);
    let mut outputs = coinbase.outputs();
    outputs[0].lock_script =
        transparent::Script::new(&vec![0; usize::try_from(block::MAX_BLOCK_BYTES).unwrap()]);
    coinbase.set_outputs(outputs);
    Arc::make_mut(&mut block_mut.header).merkle_root =
        block_mut.transactions.iter().map(|tx| tx.hash()).collect();
    assert!(u64::try_from(block.zcash_serialized_size()).unwrap() > block::MAX_BLOCK_BYTES);
    let body = Arc::downgrade(&block);
    let state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut verifier =
        CheckpointVerifier::from_list(sparse_checkpoints(), &network_without_pow(), None, state)
            .unwrap();

    let response = verifier.call(block).now_or_never();
    drop(verifier);
    assert_bodies_released(&[body]).await;
    assert!(
        matches!(
            response,
            Some(Err(VerifyCheckpointError::VerifyBlock(
                VerifyBlockError::OversizedBlock
            )))
        ),
        "internally constructed bodies must satisfy the same size bound as decoded blocks",
    );
}

#[tokio::test]
async fn sparse_pressure_preserves_cancelled_historical_range_progress() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        let blocks: Vec<_> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
            .values()
            .map(|bytes| Arc::<Block>::zcash_deserialize(*bytes).unwrap())
            .collect();
        let last = blocks.last().unwrap();
        let mut checkpoints = sparse_checkpoints();
        checkpoints.push((last.coinbase_height().unwrap(), last.hash()));
        let state = zs::init_test(&Mainnet).await;
        let mut verifier =
            CheckpointVerifier::from_list(checkpoints, &network_without_pow(), None, state.clone())
                .unwrap();
        let mut pressure_bodies = Vec::new();
        for index in 0..=RETAINED_BLOCK_LIMIT {
            let block = block_at(MAX_CHECKPOINT_HEIGHT_GAP + 2 * index, block::Hash([7; 32]));
            pressure_bodies.push(Arc::downgrade(&block));
            if let Some(result) = verifier.call(block).now_or_never() {
                assert_eq!(result.unwrap_err().misbehavior_score(), 0);
            }
        }

        // Commit genesis, then submit the rest of the checkpoint range backwards. Dropping
        // every child response must neither lose accepted blocks nor block the missing parent.
        assert_eq!(
            verifier
                .ready()
                .await
                .unwrap()
                .call(blocks[0].clone())
                .await
                .unwrap(),
            blocks[0].hash(),
        );
        for block in blocks[2..].iter().rev() {
            drop(verifier.ready().await.unwrap().call(block.clone()));
        }
        assert_eq!(
            verifier
                .ready()
                .await
                .unwrap()
                .call(blocks[1].clone())
                .await
                .unwrap(),
            blocks[1].hash(),
        );
        loop {
            let response = state.clone().oneshot(zs::Request::Tip).await.unwrap();
            if response == zs::Response::Tip(Some((last.coinbase_height().unwrap(), last.hash()))) {
                break;
            }
            tokio::task::yield_now().await;
        }
        for block in &blocks {
            assert_eq!(
                state
                    .clone()
                    .oneshot(zs::Request::Block(block.hash().into()))
                    .await
                    .unwrap(),
                zs::Response::Block(Some(block.clone())),
                "every accepted historical block must reach the real state",
            );
        }
        drop(verifier);
        assert_bodies_released(&pressure_bodies).await;
    })
    .await
    .expect("resource pressure must not obstruct a complete historical checkpoint range");
}

#[tokio::test]
async fn cancelled_checkpoint_commit_failure_still_resets_progress() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        let genesis = genesis();
        let attempts = Arc::new(AtomicUsize::new(0));
        let state_attempts = attempts.clone();
        let (failed, mut failures) = tokio::sync::mpsc::channel(1);
        let state = tower::service_fn(move |request| {
            let attempts = state_attempts.clone();
            let failed = failed.clone();
            async move {
                match request {
                    zs::Request::CommitCheckpointVerifiedBlock(block) => {
                        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            failed.send(()).await.unwrap();
                            Err::<zs::Response, BoxError>(
                                std::io::Error::other("test commit failure").into(),
                            )
                        } else {
                            Ok(zs::Response::Committed(block.hash))
                        }
                    }
                    zs::Request::Tip => Ok(zs::Response::Tip(None)),
                    _ => panic!("only commit and recovery tip requests are expected"),
                }
            }
        });
        let mut verifier = CheckpointVerifier::from_list(
            [(block::Height(0), genesis.hash())],
            &Mainnet,
            None,
            state,
        )
        .unwrap();
        drop(verifier.call(genesis.clone()));
        failures.recv().await.unwrap();

        loop {
            let mut retry = verifier.call(genesis.clone());
            match retry.as_mut().now_or_never() {
                Some(Err(VerifyCheckpointError::Finished)) => tokio::task::yield_now().await,
                Some(result) => {
                    assert_eq!(result.unwrap(), genesis.hash());
                    break;
                }
                None => {
                    assert_eq!(retry.await.unwrap(), genesis.hash());
                    break;
                }
            }
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    })
    .await
    .expect("commit failure recovery must not depend on the cancelled response future");
}

/// A recovery sender destroyed after the verifier's drop drain must resolve as `Dropped`.
#[tokio::test]
async fn late_checkpoint_sender_drop_returns_dropped() {
    let _init_guard = zebra_test::init();
    let state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let mut verifier =
        CheckpointVerifier::from_list(sparse_checkpoints(), &network_without_pow(), None, state)
            .unwrap();
    let block = block_at(MAX_CHECKPOINT_HEIGHT_GAP, block::Hash([7; 32]));
    let response = verifier.call(block.clone());
    // Model an owner returning a child after Drop has already drained the recovery channel.
    let late_child = verifier.queued.remove(&block.coinbase_height().unwrap());
    assert!(late_child.is_some());
    drop(verifier);
    drop(late_child);
    assert!(matches!(
        timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), response)
            .await
            .unwrap(),
        Err(VerifyCheckpointError::Dropped)
    ));
}

#[tokio::test]
async fn reset_cleans_stale_queue_without_releasing_running_owner() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        let (release, released) = tokio::sync::watch::channel(false);
        let (started, mut starts) = tokio::sync::mpsc::channel(1);
        let state = tower::service_fn(move |request| {
            let mut released = released.clone();
            let started = started.clone();
            async move {
                let zs::Request::CommitCheckpointVerifiedBlock(block) = request else {
                    panic!("only the accepted genesis commit is expected")
                };
                started.send(()).await.unwrap();
                released.wait_for(|released| *released).await.unwrap();
                Ok::<_, BoxError>(zs::Response::Committed(block.hash))
            }
        });
        let mut verifier = CheckpointVerifier::from_list(
            sparse_checkpoints(),
            &network_without_pow(),
            None,
            state,
        )
        .unwrap();
        let genesis = genesis();
        let running_body = Arc::downgrade(&genesis);
        drop(verifier.call(genesis));
        starts.recv().await.unwrap();
        let queued = block_at(MAX_CHECKPOINT_HEIGHT_GAP, block::Hash([7; 32]));
        let stale_body = Arc::downgrade(&queued);
        let reset_tip = Some((queued.coinbase_height().unwrap(), queued.hash()));
        drop(verifier.call(queued));
        let retained = verifier.retained_blocks.clone();
        assert_eq!(retained.load(Ordering::Relaxed), 2);

        // This fixture isolates recovery-index cleanup, not validation of the supplied state tip.
        verifier.reset_progress(reset_tip);
        assert!(verifier.queued.is_empty());
        assert_eq!(stale_body.strong_count(), 0);
        assert_ne!(running_body.strong_count(), 0);
        assert_eq!(retained.load(Ordering::Relaxed), 1);

        drop(verifier);
        release.send(true).unwrap();
        assert_bodies_released(&[running_body]).await;
        assert_eq!(retained.load(Ordering::Relaxed), 0);
    })
    .await
    .expect("reset must clean stale indexes without cancelling accepted work");
}
