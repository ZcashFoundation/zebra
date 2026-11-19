//! Finalization past the reorg limit must leave the published snapshot without pruned roots.

use chrono::{DateTime, Utc};
use zebra_chain::{serialization::ZcashDeserializeInto, work::difficulty::CompactDifficulty};

use crate::service::check::{difficulty::pow_adjustment_block_span, AdjustedDifficulty};

use super::*;

#[test]
fn early_relay_final_snapshot_excludes_finalized_roots() {
    let _init_guard = zebra_test::init();
    let (finalized, mut non_finalized, genesis) = new_state_with_mainnet_genesis();
    let mut block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let root = block.hash();
    for _ in 0..MAX_BLOCK_REORG_HEIGHT {
        non_finalized
            .commit_prevalidated_contextual(
                block.clone().prepare().test_with_zero_spent_utxos(),
                &finalized.db,
                None,
            )
            .unwrap();
        block = block.make_fake_child();
    }
    let prepared = block.clone().prepare();
    let contextual = prepared.test_with_zero_spent_utxos();
    let (mut tip_sender, latest, _) = ChainTipSender::new(
        Some(ChainTipBlock::from(genesis.clone())),
        &Network::Mainnet,
    );
    tip_sender.set_best_non_finalized_tip(ChainTipBlock::from(
        non_finalized.best_tip_block().unwrap().clone(),
    ));
    let (state_sender, snapshot) = watch::channel(non_finalized.clone());
    let (queued, non_finalized_block_write_receiver) = tokio::sync::mpsc::unbounded_channel();
    let (rsp_tx, rsp_rx) = oneshot::channel();
    queued
        .send(NonFinalizedWriteMessage::Commit(
            (prepared, rsp_tx, Some(Box::new(contextual)), None),
            Instant::now(),
        ))
        .unwrap();
    drop(queued);
    let (invalid_block_reset_sender, _resets) = tokio::sync::mpsc::unbounded_channel();
    let (non_finalized_rejected_sender, _rejections) = tokio::sync::mpsc::unbounded_channel();
    let db = finalized.db.clone();
    let worker = WriteBlockWorkerTask {
        finalized_block_write_receiver: tokio::sync::mpsc::unbounded_channel().1,
        non_finalized_block_write_receiver,
        finalized_state: finalized,
        non_finalized_state: non_finalized,
        invalid_block_reset_sender,
        non_finalized_rejected_sender,
        chain_tip_sender: tip_sender,
        non_finalized_state_sender: state_sender,
        backup_dir_path: None,
        tree_roots_cache: Some(no_fast_tree_sync()),
        pruning_wake: None,
    };
    worker.run();
    assert_eq!(rsp_rx.blocking_recv().unwrap().unwrap(), block.hash());
    assert_eq!(latest.best_tip_hash(), Some(block.hash()));
    assert_eq!(db.finalized_tip_hash(), root);
    let published = snapshot.borrow();
    assert_eq!(published.best_tip().unwrap().1, block.hash());
    assert_eq!(published.best_chain_len(), Some(MAX_BLOCK_REORG_HEIGHT));
    assert!(published
        .chain_iter()
        .all(|chain| !chain.height_by_hash.contains_key(&root)));
}

/// Returns a child of `parent` whose difficulty threshold and time pass the contextual checks,
/// and the child's recent headers.
///
/// `parent_headers` are the difficulty thresholds and times of `parent` and its ancestors, newest
/// first. Children with a different `seconds_after` are different blocks.
fn valid_child(
    parent: &Arc<Block>,
    parent_headers: &[(CompactDifficulty, DateTime<Utc>)],
    seconds_after: i64,
) -> (Arc<Block>, Vec<(CompactDifficulty, DateTime<Utc>)>) {
    let mut child = parent.make_fake_child();
    let newest_time = parent_headers
        .iter()
        .map(|(_threshold, time)| *time)
        .max()
        .expect("the parent has a header");
    let time = newest_time + chrono::Duration::seconds(seconds_after);
    Arc::make_mut(&mut Arc::make_mut(&mut child).header).time = time;

    let threshold = AdjustedDifficulty::new_from_block(
        &child,
        &Network::Mainnet,
        parent_headers.iter().copied(),
    )
    .expected_difficulty_threshold();
    Arc::make_mut(&mut Arc::make_mut(&mut child).header).difficulty_threshold = threshold;

    let mut headers = vec![(threshold, time)];
    headers.extend(parent_headers.iter().copied());
    headers.truncate(parent_headers.len());

    (child, headers)
}

/// After root blocks are finalized, the block write task must still validate the blocks it is
/// sent: on its best chain, on a side chain, and on the chain that a reorg leaves behind.
///
/// The task takes the finalized tip's height, and whether a block's parent is the finalized tip,
/// from its chains. Test builds compare both with the database each time a block is validated.
#[test]
fn blocks_are_validated_after_root_blocks_are_finalized() {
    let _init_guard = zebra_test::init();
    let (finalized, mut non_finalized, _genesis) = new_state_with_mainnet_genesis();
    let root: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();

    // A chain at the reorg limit. Every block in it has the root's difficulty threshold and time.
    let mut tip = root.clone();
    for _ in 0..MAX_BLOCK_REORG_HEIGHT {
        non_finalized
            .commit_prevalidated_contextual(
                tip.clone().prepare().test_with_zero_spent_utxos(),
                &finalized.db,
                None,
            )
            .unwrap();
        tip = tip.make_fake_child();
    }
    let tip = non_finalized.best_tip_block().unwrap().block.clone();
    let block_span = pow_adjustment_block_span(&Network::Mainnet, tip.coinbase_height().unwrap());
    let tip_headers = vec![(root.header.difficulty_threshold, root.header.time); block_span];

    // The first block past the limit finalizes the root, and so does its child.
    let (past_limit, past_limit_headers) = valid_child(&tip, &tip_headers, 1);
    let (child, child_headers) = valid_child(&past_limit, &past_limit_headers, 1);
    // A sibling of that child starts a side chain. Its own child makes the side chain the best
    // chain, which finalizes another root.
    let (side, side_headers) = valid_child(&past_limit, &past_limit_headers, 2);
    let (side_child, _headers) = valid_child(&side, &side_headers, 1);
    // The chain that the reorg left behind is extended last.
    let (grandchild, _headers) = valid_child(&child, &child_headers, 1);

    let blocks = [past_limit, child, side, side_child, grandchild];
    let (messages, responses): (Vec<_>, Vec<_>) = blocks
        .iter()
        .map(|block| {
            let (rsp_tx, rsp_rx) = oneshot::channel();
            let message = NonFinalizedWriteMessage::Commit(
                (block.clone().prepare(), rsp_tx, None, None),
                Instant::now(),
            );
            (message, rsp_rx)
        })
        .unzip();
    let db = finalized.db.clone();
    let published = prepared::run_writer(finalized, non_finalized, messages);

    for (block, mut response) in blocks.iter().zip(responses) {
        assert_eq!(response.try_recv().unwrap().unwrap(), block.hash());
    }
    // The first two blocks and the reorg each finalized a root.
    assert_eq!(db.finalized_tip_height(), Some(block::Height(3)));
    assert_eq!(published.chain_count(), 2);
}
