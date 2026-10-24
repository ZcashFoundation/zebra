//! Finalization past the reorg limit must leave the published snapshot without pruned roots.

use zebra_chain::serialization::ZcashDeserializeInto;

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
