//! Orphan retention limits and forward progress through the real state writer.

use std::{collections::HashMap, sync::Arc, time::Duration};

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
    Config, ContextuallyVerifiedBlock, SemanticallyVerifiedBlock, MAX_BLOCK_REORG_HEIGHT,
};

async fn orphan_state() -> (StateService, Arc<Block>) {
    let (mut state, _, _, _) =
        StateService::new(Config::ephemeral(), &Network::Mainnet, Height::MAX, 0).await;
    let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    state
        .queue_and_commit_to_finalized_state(genesis.into())
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

#[tokio::test(flavor = "multi_thread")]
async fn orphan_receiver_drop_retention_is_bounded() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut state, parent) = orphan_state().await;
        let child = parent.make_fake_child();
        let mut orphans = orphan_blocks(&child, MAX_BLOCK_REORG_HEIGHT);
        let highest = orphans.pop().unwrap();
        let mut evicted =
            state.queue_and_commit_to_non_finalized_state(highest.clone().prepare(), None);
        for orphan in orphans {
            drop(state.queue_and_commit_to_non_finalized_state(orphan.prepare(), None));
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
        drop(state.queue_and_commit_to_non_finalized_state(
            child.clone().prepare(),
            Some(Box::new(contextual)),
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
            state
                .queue_and_commit_to_non_finalized_state(parent.prepare(), None)
                .await
                .unwrap()
                .unwrap(),
            child.header.previous_block_hash,
        );
        while !state
            .read_service
            .latest_non_finalized_state()
            .any_chain_contains(&child.hash())
        {
            tokio::task::yield_now().await;
        }
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
                .queue_and_commit_to_non_finalized_state(ordinary.clone().prepare(), None)
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
        let (mut state, parent) = orphan_state().await;
        let child = parent.make_fake_child();
        drop(state.queue_and_commit_to_non_finalized_state(child.clone().prepare(), None));
        for orphan in orphan_blocks(&child, MAX_BLOCK_REORG_HEIGHT - 1) {
            drop(state.queue_and_commit_to_non_finalized_state(orphan.prepare(), None));
        }
        assert_eq!(
            state.non_finalized_state_queued_blocks.blocks.len(),
            usize::try_from(MAX_BLOCK_REORG_HEIGHT).unwrap(),
        );
        assert_indexes(&state.non_finalized_state_queued_blocks);
        // Duplicate replacement still occupies one slot.
        drop(state.queue_and_commit_to_non_finalized_state(child.clone().prepare(), None));
        state
            .queue_and_commit_to_non_finalized_state(parent.prepare(), None)
            .await
            .unwrap()
            .unwrap();
        while !state
            .read_service
            .latest_non_finalized_state()
            .any_chain_contains(&child.hash())
        {
            tokio::task::yield_now().await;
        }
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
        state
            .queue_and_commit_to_non_finalized_state(ordinary.clone().prepare(), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.best_tip(), Some((Height(3), ordinary.hash())));
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
        let budget =
            usize::try_from(MAX_CHECKPOINT_BYTE_COUNT).unwrap() * MAX_NON_FINALIZED_CHAIN_FORKS;
        let submissions = budget / spent_script_bytes + 1;
        assert!(submissions < usize::try_from(MAX_BLOCK_REORG_HEIGHT).unwrap());
        assert!(
            submissions * child.zcash_serialized_size() < budget,
            "block wire bytes alone must not trigger this regression",
        );
        for nonce in 0..submissions {
            let mut block = child.clone();
            let mut nonce_bytes = [0; 32];
            nonce_bytes[..8].copy_from_slice(&u64::try_from(nonce).unwrap().to_le_bytes());
            Arc::make_mut(&mut Arc::make_mut(&mut block).header).nonce = nonce_bytes.into();
            let mut cached = contextual.clone();
            cached.rebind(block, std::time::Instant::now());
            let prepared = SemanticallyVerifiedBlock {
                block: cached.block.clone(),
                hash: cached.hash,
                height: cached.height,
                new_outputs: std::mem::take(&mut cached.new_outputs),
                transaction_hashes: cached.transaction_hashes.clone(),
                received_time: cached.received_time,
            };
            drop(state.queue_and_commit_to_non_finalized_state(prepared, Some(Box::new(cached))));
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

        state
            .non_finalized_state_queued_blocks
            .prune_by_height(Height(2));
        assert!(state.non_finalized_state_queued_blocks.blocks.is_empty());
        assert_indexes(&state.non_finalized_state_queued_blocks);
        let next = parent.make_fake_child();
        drop(state.queue_and_commit_to_non_finalized_state(next.clone().prepare(), None));
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
    queue.queue((parent.clone().prepare(), sender, None));
    drop(receiver);
    let parent_bytes = queue.queued_bytes;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    queue.queue((parent.clone().prepare(), sender, None));
    drop(receiver);
    assert_eq!(queue.queued_bytes, parent_bytes);
    assert_indexes(&queue);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    queue.queue((child.clone().prepare(), sender, None));
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
    queue.queue((block.prepare(), sender, None));
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
