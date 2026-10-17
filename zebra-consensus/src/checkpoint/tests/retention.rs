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

/// Failed-range recovery and surviving owners must allow the following checkpoint range to finish.
///
/// The synthetic chain is trusted by this test's checkpoints; it is not a full-consensus fixture.
/// Commits and the notification-capacity failure run through the real finalized state writer.
#[tokio::test]
async fn failed_checkpoint_range_releases_capacity_for_real_state_recovery() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(3 * VERIFY_TIMEOUT_SECONDS), async {
        let blocks = blocks_through(RETAINED_BLOCK_LIMIT + MAX_CHECKPOINT_HEIGHT_GAP);
        let checkpoints: Vec<_> = blocks
            .iter()
            .step_by(MAX_CHECKPOINT_HEIGHT_GAP)
            .map(|block| (block.coinbase_height().unwrap(), block.hash()))
            .collect();
        let state = zs::init_test(&Mainnet).await;
        state
            .clone()
            .oneshot(zs::Request::CommitCheckpointVerifiedBlock(
                blocks[0].clone().into(),
            ))
            .await
            .unwrap();
        let initial_tip = Some((block::Height(0), blocks[0].hash()));
        let (release_descendants, descendants_released) = tokio::sync::watch::channel(false);
        let (forwarded, mut forwarded_heights) = tokio::sync::mpsc::channel(RETAINED_BLOCK_LIMIT);
        let inner_state = state.clone();
        let gated_state = tower::service_fn(move |request: zs::Request| {
            let mut state = inner_state.clone();
            let mut descendants_released = descendants_released.clone();
            let forwarded = forwarded.clone();
            async move {
                let height = match &request {
                    zs::Request::CommitCheckpointVerifiedBlock(block)
                    | zs::Request::CommitMinedCheckpointVerifiedBlock { block, .. } => {
                        Some(block.height)
                    }
                    _ => None,
                };
                let descendant = height.is_some_and(|height| height > block::Height(1));
                if descendant {
                    descendants_released
                        .wait_for(|released| *released)
                        .await
                        .unwrap();
                }
                let response = state.ready().await?.call(request);
                if descendant {
                    let _ = forwarded.try_send(height.unwrap());
                }
                response.await
            }
        });
        let mut verifier = CheckpointVerifier::from_list(
            checkpoints,
            &network_without_pow(),
            initial_tip,
            gated_state,
        )
        .unwrap();
        assert_eq!(verifier.retained_block_limit, RETAINED_BLOCK_LIMIT);

        // Force block 1 to fail at the real writer's pre-mutation boundary, then let later
        // accepted descendants arrive only after that failure has reset the state's send tip.
        let (committed, _commits) = tokio::sync::mpsc::channel(1);
        committed
            .try_send((block::Hash([9; 32]), block::Height(0)))
            .unwrap();
        let (validated, _validation) = tokio::sync::watch::channel(None);
        let first = verifier.verify(
            blocks[1].clone(),
            Some(zs::MinedBlockNotifications::new(committed, validated)),
        );
        let mut second = None;
        let mut descendants = FuturesUnordered::new();
        for block in blocks.iter().skip(2).take(RETAINED_BLOCK_LIMIT - 1) {
            let response = verifier.call(block.clone());
            if block.coinbase_height() == Some(block::Height(2)) {
                second = Some(response);
            } else {
                descendants.push(response);
            }
        }
        let error = first.await.unwrap_err();
        let VerifyCheckpointError::CommitCheckpointVerified(source) = error else {
            panic!("the first request must preserve the real state commit error")
        };
        assert!(matches!(
            source
                .downcast_ref::<zs::CommitCheckpointVerifiedError>()
                .unwrap()
                .inner(),
            zs::CommitBlockError::MinedBlockNotificationFull
        ));
        release_descendants.send(true).unwrap();

        // Either the failed range rejects its unsent suffix, or old per-block owners forward
        // height 2 after the failure. Both paths reach the same recovery attempt below.
        let mut second = second.unwrap();
        let second_resolved = loop {
            tokio::select! {
                result = &mut second => {
                    assert!(result.is_err(), "height 2 cannot commit before its failed parent");
                    break true;
                }
                Some(height) = forwarded_heights.recv() => {
                    if height == block::Height(2) {
                        break false;
                    }
                }
            }
        };
        assert_eq!(
            state.clone().oneshot(zs::Request::Tip).await.unwrap(),
            zs::Response::Tip(initial_tip),
            "the failed block and its descendants have not mutated the real chain",
        );

        let mut recovery = Vec::with_capacity(MAX_CHECKPOINT_HEIGHT_GAP);
        for block in blocks.iter().skip(1).take(MAX_CHECKPOINT_HEIGHT_GAP) {
            let mut response = verifier.call(block.clone());
            let immediate = response.as_mut().now_or_never();
            assert!(
                immediate.is_none(),
                "recovery block {:?} must fit under the unchanged cap: {immediate:?}",
                block.coinbase_height(),
            );
            recovery.push((block.hash(), response));
        }
        for (hash, response) in recovery {
            assert_eq!(response.await.unwrap(), hash);
        }
        let recovered_tip = &blocks[MAX_CHECKPOINT_HEIGHT_GAP];
        assert_eq!(
            state
                .clone()
                .oneshot(zs::Request::Block(recovered_tip.hash().into()))
                .await
                .unwrap(),
            zs::Response::Block(Some(recovered_tip.clone())),
        );
        if !second_resolved {
            let _ = second.await;
        }
        while descendants.next().await.is_some() {}
        assert_eq!(
            verifier.retained_blocks.load(Ordering::Relaxed),
            0,
            "every accepted response and retained owner must finish after recovery",
        );

        let surviving_tip = &blocks[RETAINED_BLOCK_LIMIT];
        assert_eq!(
            state.clone().oneshot(zs::Request::Tip).await.unwrap(),
            zs::Response::Tip(Some((
                surviving_tip.coinbase_height().unwrap(),
                surviving_tip.hash(),
            ))),
            "surviving pre-reset owners must have committed through height 2,000",
        );
        let next_range: Vec<_> = blocks
            .iter()
            .skip(RETAINED_BLOCK_LIMIT + 1)
            .map(|block| (block.hash(), verifier.call(block.clone())))
            .collect();
        timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
            for (hash, response) in next_range {
                assert_eq!(response.await.unwrap(), hash);
            }
        })
        .await
        .expect("surviving committed ranges must not strand verification of heights 2,001–2,400");
        let next_tip = blocks.last().unwrap();
        assert_eq!(
            state.oneshot(zs::Request::Tip).await.unwrap(),
            zs::Response::Tip(Some(
                (next_tip.coinbase_height().unwrap(), next_tip.hash(),)
            )),
        );
        assert_eq!(verifier.retained_blocks.load(Ordering::Relaxed), 0);
    })
    .await
    .expect("a failed checkpoint range must not strand the capacity needed to retry it");
}

#[derive(Clone, Copy)]
enum ReplacementObserver {
    Healthy,
    Full,
    Closed,
}

async fn ordinary_checkpoint_replacement(observer: ReplacementObserver) {
    timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        let genesis = genesis();
        let child =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])
                .unwrap();
        let state = zs::init_test(&Mainnet).await;
        let mut verifier = CheckpointVerifier::from_list(
            [
                (block::Height(0), genesis.hash()),
                (block::Height(1), child.hash()),
            ],
            &Mainnet,
            None,
            state.clone(),
        )
        .unwrap();
        let (committed, mut commits) = tokio::sync::mpsc::channel(1);
        let existing = (block::Hash([9; 32]), block::Height(0));
        match observer {
            ReplacementObserver::Healthy => {}
            ReplacementObserver::Full => committed.try_send(existing).unwrap(),
            ReplacementObserver::Closed => commits.close(),
        }
        let (validated, validation) = tokio::sync::watch::channel(None);
        drop(verifier.verify(
            child.clone(),
            Some(zs::MinedBlockNotifications::new(committed, validated)),
        ));
        let replacement = verifier.call(child.clone());
        assert_eq!(
            verifier.call(genesis.clone()).await.unwrap(),
            genesis.hash()
        );
        assert_eq!(replacement.await.unwrap(), child.hash());
        assert_eq!(
            state
                .oneshot(zs::Request::Block(child.hash().into()))
                .await
                .unwrap(),
            zs::Response::Block(Some(child.clone())),
        );
        assert!(validation.borrow().is_none());
        match observer {
            ReplacementObserver::Healthy => assert_eq!(
                commits.recv().await,
                Some((child.hash(), block::Height(1))),
                "ordinary replacement must retain the healthy mined observer",
            ),
            ReplacementObserver::Full => {
                assert_eq!(commits.recv().await, Some(existing));
                assert!(commits.try_recv().is_err());
            }
            ReplacementObserver::Closed => assert!(commits.recv().await.is_none()),
        }
    })
    .await
    .expect("a superseded mined observer must not gate the ordinary replacement");
}

#[tokio::test]
async fn ordinary_checkpoint_duplicate_preserves_healthy_mined_observer() {
    let _init_guard = zebra_test::init();
    ordinary_checkpoint_replacement(ReplacementObserver::Healthy).await;
}

#[tokio::test]
async fn ordinary_checkpoint_duplicate_ignores_unavailable_mined_observers() {
    let _init_guard = zebra_test::init();
    ordinary_checkpoint_replacement(ReplacementObserver::Full).await;
    ordinary_checkpoint_replacement(ReplacementObserver::Closed).await;
}

#[tokio::test]
async fn failed_checkpoint_range_preserves_committed_prefix_and_first_error() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        let blocks: Vec<_> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
            .values()
            .map(|bytes| Arc::<Block>::zcash_deserialize(*bytes).unwrap())
            .collect();
        let state = zs::init_test(&Mainnet).await;
        state
            .clone()
            .oneshot(zs::Request::CommitCheckpointVerifiedBlock(
                blocks[0].clone().into(),
            ))
            .await
            .unwrap();
        let initial_tip = (block::Height(0), blocks[0].hash());
        let last = blocks.last().unwrap();
        let mut verifier = CheckpointVerifier::from_list(
            [initial_tip, (last.coinbase_height().unwrap(), last.hash())],
            &Mainnet,
            Some(initial_tip),
            state.clone(),
        )
        .unwrap();
        let (committed, _commits) = tokio::sync::mpsc::channel(1);
        committed
            .try_send((block::Hash([9; 32]), block::Height(0)))
            .unwrap();
        let (validated, _validation) = tokio::sync::watch::channel(None);
        let mut notifications = Some(zs::MinedBlockNotifications::new(committed, validated));
        let mut responses = Vec::new();
        for block in blocks.iter().skip(1) {
            let height = block.coinbase_height().unwrap();
            let observer = if height == block::Height(3) {
                notifications.take()
            } else {
                None
            };
            responses.push((
                height,
                block.hash(),
                verifier.verify(block.clone(), observer),
            ));
        }
        for (height, hash, response) in responses {
            let result = response.await;
            if height < block::Height(3) {
                assert_eq!(result.unwrap(), hash);
            } else if height == block::Height(3) {
                let VerifyCheckpointError::CommitCheckpointVerified(source) = result.unwrap_err()
                else {
                    panic!("the first failing block must retain its actual state error")
                };
                assert!(matches!(
                    source
                        .downcast_ref::<zs::CommitCheckpointVerifiedError>()
                        .unwrap()
                        .inner(),
                    zs::CommitBlockError::MinedBlockNotificationFull
                ));
            } else {
                let error = result.unwrap_err();
                assert!(matches!(
                    error,
                    VerifyCheckpointError::RangeAborted { failed_hash }
                        if failed_hash == blocks[3].hash()
                ));
                let error = crate::RouterError::from(error);
                assert!(error.is_resource_pressure());
                assert!(!error.is_duplicate_request());
                assert!(!error.is_auth_commitment_mismatch());
                assert_eq!(error.misbehavior_score(), 0);
            }
        }
        assert_eq!(
            state.clone().oneshot(zs::Request::Tip).await.unwrap(),
            zs::Response::Tip(Some((block::Height(2), blocks[2].hash()))),
            "the committed prefix must survive the failed range",
        );
        assert_eq!(verifier.retained_blocks.load(Ordering::Relaxed), 0);
        let recovery: Vec<_> = blocks
            .iter()
            .skip(3)
            .map(|block| (block.hash(), verifier.call(block.clone())))
            .collect();
        for (hash, response) in recovery {
            assert_eq!(response.await.unwrap(), hash);
        }
        assert_eq!(
            state.oneshot(zs::Request::Tip).await.unwrap(),
            zs::Response::Tip(Some((last.coinbase_height().unwrap(), last.hash()))),
        );
    })
    .await
    .expect("range recovery must retain the committed prefix and resolve every suffix response");
}

#[tokio::test]
async fn delayed_checkpoint_completion_and_reset_do_not_rewind_confirmed_progress() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        let blocks: Vec<_> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
            .values()
            .take(5)
            .map(|bytes| Arc::<Block>::zcash_deserialize(*bytes).unwrap())
            .collect();
        let state = zs::init_test(&Mainnet).await;
        state
            .clone()
            .oneshot(zs::Request::CommitCheckpointVerifiedBlock(
                blocks[0].clone().into(),
            ))
            .await
            .unwrap();
        let initial_tip = Some((block::Height(0), blocks[0].hash()));
        let (release, released) = tokio::sync::watch::channel(0);
        let (committed, mut commits) = tokio::sync::mpsc::channel(2);
        let inner_state = state.clone();
        let gated_state = tower::service_fn(move |request: zs::Request| {
            let mut state = inner_state.clone();
            let mut released = released.clone();
            let committed = committed.clone();
            async move {
                let height = match &request {
                    zs::Request::CommitCheckpointVerifiedBlock(block) => Some(block.height),
                    _ => None,
                };
                let response = state.ready().await?.call(request).await?;
                if let Some(height) = height.filter(|height| height.0 <= 2) {
                    committed.send(height).await.unwrap();
                    let target = height.0;
                    released
                        .wait_for(move |released| *released >= target)
                        .await
                        .unwrap();
                }
                Ok::<_, BoxError>(response)
            }
        });
        let mut verifier = CheckpointVerifier::from_list(
            [0, 2, 3, 4].map(|height| {
                (
                    blocks[height].coinbase_height().unwrap(),
                    blocks[height].hash(),
                )
            }),
            &Mainnet,
            initial_tip,
            gated_state,
        )
        .unwrap();
        let first = verifier.call(blocks[1].clone());
        let second = verifier.call(blocks[2].clone());
        let third = verifier.call(blocks[3].clone());
        assert_eq!(commits.recv().await, Some(block::Height(1)));
        assert_eq!(
            state.clone().oneshot(zs::Request::Tip).await.unwrap(),
            zs::Response::Tip(Some((block::Height(1), blocks[1].hash()))),
        );
        assert_eq!(
            *verifier.committed_receiver.borrow(),
            None,
            "a committed prefix must not publish its unfinished checkpoint target",
        );

        release.send(1).unwrap();
        assert_eq!(first.await.unwrap(), blocks[1].hash());
        assert_eq!(commits.recv().await, Some(block::Height(2)));
        assert_eq!(third.await.unwrap(), blocks[3].hash());
        assert_eq!(
            *verifier.committed_receiver.borrow(),
            Some(block::Height(3))
        );
        assert!(matches!(
            verifier.call(blocks[3].clone()).await,
            Err(VerifyCheckpointError::AlreadyVerified { .. })
        ));
        assert!(!verifier.committed_receiver.has_changed().unwrap());

        // The older range's last reply is delivered after a higher checkpoint has committed.
        release.send(2).unwrap();
        assert_eq!(second.await.unwrap(), blocks[2].hash());
        assert_eq!(
            *verifier.committed_receiver.borrow(),
            Some(block::Height(3))
        );
        assert!(!verifier.committed_receiver.has_changed().unwrap());

        // Model a recovery-tip query whose delivery lagged both successful ranges. The confirmed
        // watch is unchanged, but the next call must still clamp this reset forward before admission.
        verifier.reset_sender.send(initial_tip).unwrap();
        assert_eq!(
            verifier.call(blocks[4].clone()).await.unwrap(),
            blocks[4].hash()
        );
        assert_eq!(
            state.oneshot(zs::Request::Tip).await.unwrap(),
            zs::Response::Tip(Some((block::Height(4), blocks[4].hash()))),
        );
        assert_eq!(verifier.retained_blocks.load(Ordering::Relaxed), 0);
    })
    .await
    .expect("late checkpoint replies and resets must not block the next real-state range");
}
