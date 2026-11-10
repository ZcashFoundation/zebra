//! Orphan retention limits and forward progress through the real state writer.

use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::sync::{mpsc, watch};

use zebra_chain::{
    amount::DeferredPoolBalanceChange,
    block::{Block, Height},
    parameters::{checkpoint::constants::MAX_CHECKPOINT_BYTE_COUNT, Network},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transparent,
};

use crate::{
    arbitrary::Prepare,
    constants::MAX_NON_FINALIZED_CHAIN_FORKS,
    service::{queued_blocks::QueuedBlocks, StateService},
    tests::FakeChainHelper,
    Config, ContextuallyVerifiedBlock, MinedBlockNotifications, Request, MAX_BLOCK_REORG_HEIGHT,
};

async fn orphan_state() -> (StateService, Arc<Block>) {
    let (mut state, _, _, _) =
        StateService::new(Config::ephemeral(), &Network::Mainnet, Height::MAX, 0).await;
    let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    state
        .queue_and_commit_to_finalized_state(genesis.into(), None)
        .await
        .unwrap()
        .unwrap();
    let parent = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    (state, parent)
}

/// Omit every other block so each queued block waits for a distinct absent parent.
fn orphan_blocks(first: &Arc<Block>, count: u32) -> Vec<Arc<Block>> {
    let mut previous = first.clone();
    (0..count)
        .map(|_| {
            let mut missing_parent = previous.make_fake_child();
            // Keep filler ancestry separate from the ordinary successor used by progress controls.
            Arc::make_mut(&mut Arc::make_mut(&mut missing_parent).header)
                .nonce
                .0[0] ^= 1;
            previous = missing_parent.make_fake_child();
            previous.clone()
        })
        .collect()
}

fn assert_secondary_indexes(queue: &QueuedBlocks) {
    assert_eq!(
        queue
            .by_parent
            .values()
            .map(|hashes| hashes.len())
            .sum::<usize>(),
        queue.blocks.len(),
    );
    assert_eq!(
        queue
            .by_height
            .values()
            .map(|hashes| hashes.len())
            .sum::<usize>(),
        queue.blocks.len(),
    );
    assert!(queue.by_parent.values().all(|hashes| !hashes.is_empty()));
    assert!(queue.by_height.values().all(|hashes| !hashes.is_empty()));
    assert_eq!(
        queue.queued_bytes,
        queue.blocks.values().map(|(_, bytes)| bytes).sum::<usize>(),
        "every retained payload must have exactly one resource charge",
    );
    for (hash, (queued, _)) in &queue.blocks {
        assert!(queue.by_parent[&queued.0.block.header.previous_block_hash].contains(hash));
        assert!(queue.by_height[&queued.0.height].contains(hash));
    }
}

fn assert_indexes(queue: &QueuedBlocks) {
    assert_secondary_indexes(queue);
    // These fixtures have distinct coinbase heights and therefore disjoint output sets.
    let outputs: HashMap<_, _> = queue
        .blocks
        .values()
        .flat_map(|(queued, _)| {
            queued
                .0
                .new_outputs
                .iter()
                .map(|(outpoint, output)| (*outpoint, output.utxo.clone()))
        })
        .collect();
    let known: HashMap<_, _> = queue
        .known_utxos
        .0
        .iter()
        .map(|(outpoint, (utxo, _creators))| (*outpoint, utxo.clone()))
        .collect();
    assert_eq!(known, outputs);
}

fn assert_no_orphan_indexing(queue: &QueuedBlocks) {
    assert_indexes(queue);
    assert_eq!(queue.queued_bytes, 0);
    assert_eq!(
        queue.blocks.capacity(),
        0,
        "ready blocks must never be queued"
    );
    assert_eq!(queue.by_parent.capacity(), 0);
    assert!(queue.by_height.is_empty());
    assert_eq!(queue.known_utxos.0.capacity(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn ready_commit_skips_orphan_indexes_and_keeps_sent_outputs() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut state, parent) = orphan_state().await;
        state
            .queue_and_commit_to_non_finalized_state(parent.clone().prepare(), None, None)
            .await
            .unwrap()
            .unwrap();
        // The checkpoint transition retains its queue path.
        state.non_finalized_state_queued_blocks = QueuedBlocks::default();
        let first = parent.make_fake_child();
        let second = first.clone().set_block_commitment([1; 32]);
        let prepared = first.clone().prepare();
        let shared = *prepared.new_outputs.keys().next().unwrap();
        let (committed, mut commits) = mpsc::channel(2);
        let (validated, bodies) = watch::channel(None);
        for block in [&first, &second] {
            drop(state.queue_and_commit_to_non_finalized_state(
                block.clone().prepare(),
                None,
                Some(MinedBlockNotifications::new(
                    committed.clone(),
                    validated.clone(),
                )),
            ));
            assert_no_orphan_indexing(&state.non_finalized_state_queued_blocks);
            assert!(state
                .non_finalized_block_write_sent_hashes
                .contains(&block.hash()));
            assert_eq!(commits.recv().await, Some((block.hash(), Height(2))));
            assert!(state
                .read_service
                .latest_non_finalized_state()
                .any_chain_contains(&block.hash()));
        }
        assert!(bodies.borrow().is_none());
        let duplicate = state
            .queue_and_commit_to_non_finalized_state(first.clone().prepare(), None, None)
            .await
            .unwrap()
            .unwrap_err();
        assert!(duplicate.inner().is_duplicate_request());
        state
            .non_finalized_block_write_sent_hashes
            .remove(&first.hash());
        assert!(state
            .non_finalized_block_write_sent_hashes
            .utxo(&shared)
            .is_some());
        state
            .non_finalized_block_write_sent_hashes
            .remove(&second.hash());
        assert!(state
            .non_finalized_block_write_sent_hashes
            .utxo(&shared)
            .is_none());
    })
    .await
    .expect("ready siblings must commit despite dropped response receivers");
}

#[tokio::test(flavor = "multi_thread")]
async fn ready_commit_contextual_failure_can_retry_without_orphan_indexes() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut state, parent) = orphan_state().await;
        state
            .queue_and_commit_to_non_finalized_state(parent.clone().prepare(), None, None)
            .await
            .unwrap()
            .unwrap();
        state.non_finalized_state_queued_blocks = QueuedBlocks::default();
        let child = parent.make_fake_child();
        let mut invalid = child.clone().prepare();
        invalid.height = Height(3);
        let (committed, mut commits) = mpsc::channel(1);
        let (validated, bodies) = watch::channel(None);
        let error = state
            .queue_and_commit_to_non_finalized_state(
                invalid,
                None,
                Some(MinedBlockNotifications::new(committed.clone(), validated)),
            )
            .await
            .unwrap()
            .unwrap_err();
        assert!(!error.inner().is_duplicate_request());
        assert_eq!(state.best_tip(), Some((Height(1), parent.hash())));
        assert!(commits.try_recv().is_err());
        assert!(bodies.borrow().is_none());
        assert_eq!(committed.capacity(), 1);
        assert_no_orphan_indexing(&state.non_finalized_state_queued_blocks);
        assert_eq!(
            state
                .queue_and_commit_to_non_finalized_state(child.clone().prepare(), None, None)
                .await
                .unwrap()
                .unwrap(),
            child.hash(),
        );
        assert_eq!(state.best_tip(), Some((Height(2), child.hash())));
        assert_no_orphan_indexing(&state.non_finalized_state_queued_blocks);
    })
    .await
    .expect("a rejected ready block must not poison same-hash retry");
}

#[tokio::test(flavor = "multi_thread")]
async fn ready_commit_writer_shutdown_clears_orphans() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut state, parent) = orphan_state().await;
        state
            .queue_and_commit_to_non_finalized_state(parent.clone().prepare(), None, None)
            .await
            .unwrap()
            .unwrap();
        state.non_finalized_state_queued_blocks = QueuedBlocks::default();
        let child = parent.make_fake_child();
        let descendant = child.make_fake_child();
        let waiting =
            state.queue_and_commit_to_non_finalized_state(descendant.prepare(), None, None);
        let (sender, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        state.block_write_sender.non_finalized = Some(sender);
        let response =
            state.queue_and_commit_to_non_finalized_state(child.clone().prepare(), None, None);
        for response in [response, waiting] {
            assert_eq!(
                response.await.unwrap().unwrap_err().inner(),
                &crate::CommitBlockError::WriteTaskExited,
            );
        }
        assert!(state.non_finalized_state_queued_blocks.blocks.is_empty());
        assert_indexes(&state.non_finalized_state_queued_blocks);
        state.non_finalized_state_queued_blocks = QueuedBlocks::default();
        let sibling = child.set_block_commitment([1; 32]);
        assert_eq!(
            state
                .queue_and_commit_to_non_finalized_state(sibling.prepare(), None, None)
                .await
                .unwrap()
                .unwrap_err()
                .inner(),
            &crate::CommitBlockError::WriteTaskExited,
        );
        assert_no_orphan_indexing(&state.non_finalized_state_queued_blocks);
    })
    .await
    .expect("writer shutdown must answer ready and orphaned callers");
}

#[tokio::test(flavor = "multi_thread")]
async fn orphan_receiver_drop_retention_is_bounded() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut state, parent) = orphan_state().await;
        let child = parent.make_fake_child();
        let mut orphans = orphan_blocks(&child, MAX_BLOCK_REORG_HEIGHT);
        let highest = orphans.pop().unwrap();
        let mut evicted =
            state.queue_and_commit_to_non_finalized_state(highest.clone().prepare(), None, None);
        for orphan in orphans {
            drop(state.queue_and_commit_to_non_finalized_state(orphan.prepare(), None, None));
        }
        assert_eq!(
            state.non_finalized_state_queued_blocks.blocks.len(),
            usize::try_from(MAX_BLOCK_REORG_HEIGHT).unwrap(),
        );
        assert_indexes(&state.non_finalized_state_queued_blocks);

        let mut proposal_state = state.read_service.latest_non_finalized_state();
        proposal_state.disable_metrics();
        proposal_state
            .commit_new_chain(parent.clone().prepare(), &state.read_service.db)
            .unwrap();
        proposal_state
            .commit_block(child.clone().prepare(), &state.read_service.db)
            .unwrap();
        let contextual = proposal_state.best_tip_block().unwrap().clone();
        let (committed, mut commits) = mpsc::channel(1);
        let (validated, bodies) = watch::channel(None);
        drop(state.queue_and_commit_to_non_finalized_state(
            child.clone().prepare(),
            Some(Box::new(contextual)),
            Some(MinedBlockNotifications::new(committed.clone(), validated)),
        ));

        let queue = &state.non_finalized_state_queued_blocks;
        assert_eq!(
            queue.blocks.len(),
            usize::try_from(MAX_BLOCK_REORG_HEIGHT).unwrap(),
            "dropped response receivers must not permit unbounded orphan retention",
        );
        assert!(queue.has(child.hash()));
        assert!(!queue.has(highest.hash()));
        assert!(queue
            .blocks
            .values()
            .all(|(queued, _)| queued.1.is_closed()));
        assert_indexes(queue);
        let error = evicted
            .try_recv()
            .expect("capacity eviction must answer the affected caller")
            .expect_err("the highest orphan must be evicted for the lower ancestor");
        assert_eq!(error.inner(), &crate::CommitBlockError::OrphanQueueFull);
        assert_eq!(error.inner().misbehavior_score(), 0);
        assert!(!error.inner().is_duplicate_request());
        assert!(!error.inner().is_auth_commitment_mismatch());
        assert!(!error.inner().is_descendant_of_auth_commitment_mismatch());
        assert_eq!(
            committed.capacity(),
            1,
            "orphans must not hold relay permits"
        );
        assert!(commits.try_recv().is_err());
        assert!(bodies.borrow().is_none());

        // A duplicate replaces only the response and notification policy, not the accepted payload.
        let mut replacement = child.clone().prepare();
        replacement.height = Height::MAX;
        replacement.new_outputs.clear();
        let response = state.queue_and_commit_to_non_finalized_state(replacement, None, None);

        assert_eq!(
            state
                .queue_and_commit_to_non_finalized_state(parent.prepare(), None, None)
                .await
                .unwrap()
                .unwrap(),
            child.header.previous_block_hash,
        );
        assert_eq!(response.await.unwrap().unwrap(), child.hash());
        assert_eq!(commits.recv().await, Some((child.hash(), Height(2))));
        assert_eq!(*bodies.borrow(), Some(child.clone()));
        assert!(state
            .read_service
            .latest_non_finalized_state()
            .any_chain_contains(&child.hash()));
        assert!(!state.non_finalized_state_queued_blocks.has(child.hash()));
        assert_indexes(&state.non_finalized_state_queued_blocks);

        let ordinary = child.make_fake_child();
        assert!(
            !state
                .non_finalized_state_queued_blocks
                .has_queued_children(ordinary.hash()),
            "filler orphans must not become ready when the ordinary successor arrives",
        );
        assert_eq!(
            state
                .queue_and_commit_to_non_finalized_state(ordinary.clone().prepare(), None, None)
                .await
                .unwrap()
                .unwrap(),
            ordinary.hash(),
        );
        assert_eq!(state.best_tip(), Some((Height(3), ordinary.hash())));
        state
            .non_finalized_state_queued_blocks
            .prune_by_height(highest.coinbase_height().unwrap());
        assert!(state.non_finalized_state_queued_blocks.blocks.is_empty());
        assert_indexes(&state.non_finalized_state_queued_blocks);
    })
    .await
    .expect("bounded orphan eviction and retained child commits must finish");
}

#[tokio::test(flavor = "multi_thread")]
async fn full_orphan_queue_does_not_block_ready_parent() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut state, root) = orphan_state().await;
        state
            .queue_and_commit_to_non_finalized_state(root.clone().prepare(), None, None)
            .await
            .unwrap()
            .unwrap();
        let parent = root.make_fake_child();
        let child = parent.make_fake_child();
        let (committed, mut commits) = mpsc::channel(1);
        let (validated, bodies) = watch::channel(None);
        drop(state.queue_and_commit_to_non_finalized_state(
            child.clone().prepare(),
            None,
            Some(MinedBlockNotifications::new(committed.clone(), validated)),
        ));
        let mut sibling = child.clone();
        Arc::make_mut(&mut Arc::make_mut(&mut sibling).header).previous_block_hash =
            [0xee; 32].into();
        let shared = *child.clone().prepare().new_outputs.keys().next().unwrap();
        drop(state.queue_and_commit_to_non_finalized_state(sibling.clone().prepare(), None, None));
        for orphan in orphan_blocks(&child, MAX_BLOCK_REORG_HEIGHT - 2) {
            drop(state.queue_and_commit_to_non_finalized_state(orphan.prepare(), None, None));
        }
        assert_eq!(
            state.non_finalized_state_queued_blocks.blocks.len(),
            usize::try_from(MAX_BLOCK_REORG_HEIGHT).unwrap(),
        );
        assert_indexes(&state.non_finalized_state_queued_blocks);
        assert_eq!(committed.capacity(), 1);
        assert!(commits.try_recv().is_err());
        assert!(bodies.borrow().is_none());
        // Duplicate replacement is still one slot and preserves accepted mined notifications.
        drop(state.queue_and_commit_to_non_finalized_state(child.clone().prepare(), None, None));
        state
            .queue_and_commit_to_non_finalized_state(parent.prepare(), None, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(commits.recv().await, Some((child.hash(), Height(3))));
        assert!(state
            .read_service
            .latest_non_finalized_state()
            .any_chain_contains(&child.hash()));
        assert!(
            bodies.borrow().is_none(),
            "uncached blocks cannot relay early"
        );
        assert!(!state.non_finalized_state_queued_blocks.has(child.hash()));
        assert_indexes(&state.non_finalized_state_queued_blocks);
        assert!(state.non_finalized_state_queued_blocks.has(sibling.hash()));
        assert!(state
            .non_finalized_state_queued_blocks
            .utxo(&shared)
            .is_some());
        let ordinary = child.make_fake_child();
        assert!(
            !state
                .non_finalized_state_queued_blocks
                .has_queued_children(ordinary.hash()),
            "filler orphans must not become ready when the ordinary successor arrives",
        );
        state
            .queue_and_commit_to_non_finalized_state(ordinary.clone().prepare(), None, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.best_tip(), Some((Height(4), ordinary.hash())));
    })
    .await
    .expect("a full orphan queue must not block its ready parent or ordinary commits");
}

#[tokio::test(flavor = "multi_thread")]
async fn cached_orphan_spent_outputs_share_the_byte_bound() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut state, parent) = orphan_state().await;

        // A spendable script below the 10,000-byte limit: push/drop 500-byte elements,
        // then OP_TRUE. Every cached output is referenced by a real transaction input.
        let mut script = Vec::new();
        for _ in 0..19 {
            script.extend([0x4d, 0xf4, 0x01]); // OP_PUSHDATA2, 500 bytes
            script.extend([0; 500]);
            script.push(0x75); // OP_DROP
        }
        script.push(0x51); // OP_TRUE
        let output =
            transparent::Output::new(1.try_into().unwrap(), transparent::Script::new(&script));
        let funding = parent.transactions[0]
            .as_ref()
            .clone()
            .with_transparent_inputs(vec![transparent::Input::PrevOut {
                outpoint: transparent::OutPoint {
                    hash: [7; 32].into(),
                    index: 0,
                },
                unlock_script: transparent::Script::new(&[]),
                sequence: u32::MAX,
            }])
            .with_transparent_outputs(vec![output; 64]);
        let funding_hash = funding.hash();
        let spent_outputs: HashMap<_, _> = funding
            .outputs()
            .iter()
            .enumerate()
            .map(|(index, output)| {
                (
                    transparent::OutPoint::from_usize(funding_hash, index),
                    transparent::OrderedUtxo::new(output.clone(), Height(1), 1),
                )
            })
            .collect();
        let inputs = spent_outputs
            .keys()
            .map(|outpoint| transparent::Input::PrevOut {
                outpoint: *outpoint,
                unlock_script: transparent::Script::new(&[]),
                sequence: u32::MAX,
            })
            .collect();
        let spending = funding
            .clone()
            .with_transparent_inputs(inputs)
            .with_transparent_outputs(vec![transparent::Output::new(
                64.try_into().unwrap(),
                transparent::Script::new(&[0x51]),
            )]);
        assert_eq!(spending.inputs().len(), spent_outputs.len());
        let spent_script_bytes: usize = spent_outputs
            .values()
            .map(|utxo| utxo.utxo.output.lock_script.as_raw_bytes().len())
            .sum();
        let mut child = parent.make_fake_child();
        Arc::make_mut(&mut child)
            .transactions
            .push(Arc::new(spending));
        let merkle_root = child.transactions.iter().map(|tx| tx.hash()).collect();
        Arc::make_mut(&mut Arc::make_mut(&mut child).header).merkle_root = merkle_root;
        let contextual = ContextuallyVerifiedBlock::with_block_and_spent_utxos(
            child.clone().prepare(),
            spent_outputs,
            DeferredPoolBalanceChange::zero(),
            &Network::Mainnet,
            Default::default(),
        )
        .unwrap();
        let spent_storage = Arc::downgrade(&contextual.spent_outputs);
        let budget =
            usize::try_from(MAX_CHECKPOINT_BYTE_COUNT).unwrap() * MAX_NON_FINALIZED_CHAIN_FORKS;
        let submissions = budget / spent_script_bytes + 1;
        assert!(submissions < usize::try_from(MAX_BLOCK_REORG_HEIGHT).unwrap());
        assert!(
            submissions * child.zcash_serialized_size() < budget,
            "block wire bytes alone must not trigger this regression",
        );
        let (committed, mut commits) = mpsc::channel(1);
        let (validated, bodies) = watch::channel(None);
        for nonce in 0..submissions {
            let mut block = child.clone();
            let mut nonce_bytes = [0; 32];
            nonce_bytes[..8].copy_from_slice(&u64::try_from(nonce).unwrap().to_le_bytes());
            Arc::make_mut(&mut Arc::make_mut(&mut block).header).nonce = nonce_bytes.into();
            let mut cached = contextual.clone();
            cached.rebind(block, std::time::Instant::now());
            let Request::CommitMinedBlock {
                block,
                contextual,
                notifications,
            } = cached.into_mined_request(MinedBlockNotifications::new(
                committed.clone(),
                validated.clone(),
            ))
            else {
                unreachable!("into_mined_request always creates a mined commit");
            };
            drop(state.queue_and_commit_to_non_finalized_state(
                block,
                contextual,
                Some(notifications),
            ));
        }
        drop(contextual);

        let queue = &state.non_finalized_state_queued_blocks;
        let retained_spent_bytes: usize = queue
            .blocks
            .values()
            .flat_map(|(queued, _)| queued.2.as_ref().unwrap().spent_outputs.values())
            .map(|utxo| utxo.utxo.output.lock_script.as_raw_bytes().len())
            .sum();
        assert!(
            retained_spent_bytes <= budget,
            "cached prior-output scripts must share the orphan byte budget: \
             {retained_spent_bytes} > {budget}",
        );
        assert!(!queue.blocks.is_empty());
        assert!(
            queue.blocks.len() < submissions,
            "byte pressure must evict below the count cap"
        );
        assert_eq!(spent_storage.strong_count(), queue.blocks.len());
        assert!(queue
            .blocks
            .values()
            .all(|(queued, _)| queued.1.is_closed()));
        assert_secondary_indexes(queue);
        assert!(queue.queued_bytes <= budget);
        // Evicted blocks leave no stale outputs in the cache.
        assert!(queue.known_utxos.0.keys().all(|outpoint| queue
            .blocks
            .values()
            .any(|(queued, _)| queued.0.new_outputs.contains_key(outpoint))));
        assert_eq!(committed.capacity(), 1);
        assert!(commits.try_recv().is_err());
        assert!(bodies.borrow().is_none());

        state
            .non_finalized_state_queued_blocks
            .prune_by_height(Height(2));
        assert!(state.non_finalized_state_queued_blocks.blocks.is_empty());
        assert_indexes(&state.non_finalized_state_queued_blocks);
        assert!(
            spent_storage.upgrade().is_none(),
            "pruning must release cached verdict ownership"
        );
        let next = parent.make_fake_child();
        drop(state.queue_and_commit_to_non_finalized_state(next.clone().prepare(), None, None));
        assert!(state.non_finalized_state_queued_blocks.has(next.hash()));
        assert_indexes(&state.non_finalized_state_queued_blocks);
    })
    .await
    .expect("cached orphan resource accounting must be bounded without waiting for parents");
}

#[test]
fn orphan_charge_is_released_on_duplicate_dequeue_and_drain() {
    let _init_guard = zebra_test::init();
    let parent = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    let child = parent.make_fake_child();
    let mut queue = QueuedBlocks::default();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    queue.queue((parent.clone().prepare(), sender, None, None));
    drop(receiver);
    let parent_bytes = queue.queued_bytes;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    queue.queue((parent.clone().prepare(), sender, None, None));
    drop(receiver);
    assert_eq!(queue.queued_bytes, parent_bytes);
    assert_indexes(&queue);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    queue.queue((child.clone().prepare(), sender, None, None));
    drop(receiver);
    let child_bytes = queue.queued_bytes - parent_bytes;
    assert!(child_bytes > 0);
    let dequeued = queue.dequeue_children(parent.header.previous_block_hash);
    assert_eq!(dequeued[0].0.hash, parent.hash());
    assert_eq!(queue.queued_bytes, child_bytes);
    assert_indexes(&queue);
    let drained: Vec<_> = queue.drain().collect();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].0, child.hash());
    assert_eq!(queue.queued_bytes, 0);
    assert_indexes(&queue);
}

#[test]
fn orphan_accounting_overflow_rejects_without_retention() {
    let _init_guard = zebra_test::init();
    let block = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    // Exercise arithmetic overflow without allocating an address space worth of payloads.
    let mut queue = QueuedBlocks {
        queued_bytes: usize::MAX,
        ..Default::default()
    };
    let (sender, mut receiver) = tokio::sync::oneshot::channel();
    queue.queue((block.prepare(), sender, None, None));
    assert_eq!(
        receiver.try_recv().unwrap().unwrap_err().inner(),
        &crate::CommitBlockError::OrphanQueueFull,
    );
    assert!(queue.blocks.is_empty());
    assert!(queue.by_parent.is_empty());
    assert!(queue.by_height.is_empty());
    assert!(queue.known_utxos.0.is_empty());
    assert_eq!(queue.queued_bytes, usize::MAX);
}

async fn ordinary_replacement_ignores_superseded_mined_channel(closed: bool) {
    let (mut state, parent) = orphan_state().await;
    let child = parent.make_fake_child();
    let (committed, mut commits) = mpsc::channel(1);
    let (validated, bodies) = watch::channel(None);
    if closed {
        commits.close();
    } else {
        committed.try_send((parent.hash(), Height(1))).unwrap();
    }
    let older = state.queue_and_commit_to_non_finalized_state(
        child.clone().prepare(),
        None,
        Some(MinedBlockNotifications::new(committed, validated)),
    );
    let queued_bytes = state.non_finalized_state_queued_blocks.queued_bytes;
    let ordinary =
        state.queue_and_commit_to_non_finalized_state(child.clone().prepare(), None, None);
    assert!(older
        .await
        .unwrap()
        .unwrap_err()
        .inner()
        .is_duplicate_request());
    assert_eq!(
        state.non_finalized_state_queued_blocks.queued_bytes,
        queued_bytes
    );
    assert_indexes(&state.non_finalized_state_queued_blocks);
    state
        .queue_and_commit_to_non_finalized_state(parent.clone().prepare(), None, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        ordinary.await.unwrap().expect(
            "an ordinary replacement must not inherit a superseded mined notification requirement",
        ),
        child.hash(),
    );
    assert!(state
        .read_service
        .latest_non_finalized_state()
        .any_chain_contains(&child.hash()));
    if !closed {
        assert_eq!(commits.try_recv().unwrap(), (parent.hash(), Height(1)));
    }
    assert!(
        commits.try_recv().is_err(),
        "the superseded request must not receive a commit"
    );
    assert!(bodies.borrow().is_none());
    assert_indexes(&state.non_finalized_state_queued_blocks);
    let next = child.make_fake_child();
    state
        .queue_and_commit_to_non_finalized_state(next.clone().prepare(), None, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.best_tip(), Some((Height(3), next.hash())));
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_duplicate_does_not_inherit_full_mined_channel() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(
        Duration::from_secs(10),
        ordinary_replacement_ignores_superseded_mined_channel(false),
    )
    .await
    .expect("ordinary replacement must progress despite the superseded full mined channel");
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_duplicate_does_not_inherit_closed_mined_channel() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(
        Duration::from_secs(10),
        ordinary_replacement_ignores_superseded_mined_channel(true),
    )
    .await
    .expect("ordinary replacement must progress despite the superseded closed mined channel");
}

async fn ordinary_checkpoint_replacement_ignores_superseded_mined_channel(closed: bool) {
    let (mut state, _, _, _) =
        StateService::new(Config::ephemeral(), &Network::Mainnet, Height::MAX, 0).await;
    let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    let child = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    let (committed, mut commits) = mpsc::channel(1);
    let (validated, bodies) = watch::channel(None);
    if closed {
        commits.close();
    } else {
        committed.try_send((genesis.hash(), Height(0))).unwrap();
    }
    let older = state.queue_and_commit_to_finalized_state(
        child.clone().into(),
        Some(MinedBlockNotifications::new(committed, validated)),
    );
    let ordinary = state.queue_and_commit_to_finalized_state(child.clone().into(), None);
    assert!(older
        .await
        .unwrap()
        .unwrap_err()
        .inner()
        .is_duplicate_request());
    assert_eq!(state.finalized_state_queued_blocks.len(), 1);
    state
        .queue_and_commit_to_finalized_state(genesis.clone().into(), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        ordinary.await.unwrap().expect(
            "an ordinary checkpoint replacement must not inherit a superseded mined requirement",
        ),
        child.hash(),
    );
    assert_eq!(state.read_service.db.finalized_tip_hash(), child.hash());
    if !closed {
        assert_eq!(commits.try_recv().unwrap(), (genesis.hash(), Height(0)));
    }
    assert!(commits.try_recv().is_err());
    assert!(bodies.borrow().is_none());
    assert!(state.finalized_state_queued_blocks.is_empty());
    let next = child.make_fake_child();
    state
        .queue_and_commit_to_finalized_state(next.clone().into(), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.read_service.db.finalized_tip_hash(), next.hash());
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_checkpoint_duplicate_does_not_inherit_full_mined_channel() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(
        Duration::from_secs(10),
        ordinary_checkpoint_replacement_ignores_superseded_mined_channel(false),
    )
    .await
    .expect("ordinary checkpoint replacement must progress despite the superseded full channel");
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_checkpoint_duplicate_does_not_inherit_closed_mined_channel() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(
        Duration::from_secs(10),
        ordinary_checkpoint_replacement_ignores_superseded_mined_channel(true),
    )
    .await
    .expect("ordinary checkpoint replacement must progress despite the superseded closed channel");
}

#[tokio::test(flavor = "multi_thread")]
async fn newer_mined_duplicate_restores_required_notifications() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut state, parent) = orphan_state().await;
        let child = parent.make_fake_child();
        let (committed, mut commits) = mpsc::channel(1);
        let (validated, bodies) = watch::channel(None);
        committed.try_send((parent.hash(), Height(1))).unwrap();
        let notifications = MinedBlockNotifications::new(committed, validated);
        let older = state.queue_and_commit_to_non_finalized_state(
            child.clone().prepare(),
            None,
            Some(notifications.clone()),
        );
        let ordinary =
            state.queue_and_commit_to_non_finalized_state(child.clone().prepare(), None, None);
        assert!(older
            .await
            .unwrap()
            .unwrap_err()
            .inner()
            .is_duplicate_request());
        let mined = state.queue_and_commit_to_non_finalized_state(
            child.clone().prepare(),
            None,
            Some(notifications),
        );
        assert!(ordinary
            .await
            .unwrap()
            .unwrap_err()
            .inner()
            .is_duplicate_request());
        state
            .queue_and_commit_to_non_finalized_state(parent.clone().prepare(), None, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            mined.await.unwrap().unwrap_err().inner(),
            &crate::CommitBlockError::MinedBlockNotificationFull,
        );
        assert!(!state
            .read_service
            .latest_non_finalized_state()
            .any_chain_contains(&child.hash()));
        assert!(bodies.borrow().is_none());
        assert_eq!(commits.try_recv().unwrap(), (parent.hash(), Height(1)));
        state
            .queue_and_commit_to_non_finalized_state(child.clone().prepare(), None, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.best_tip(), Some((Height(2), child.hash())));
    })
    .await
    .expect(
        "a newer mined request must restore required admission without poisoning ordinary retry",
    );
}
