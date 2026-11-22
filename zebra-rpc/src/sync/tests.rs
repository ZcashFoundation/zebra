//! Regression tests for secondary finalized-tip publication.

use std::{sync::Arc, time::Duration};

use tokio::sync::{broadcast, watch, Notify};
use zebra_chain::{
    amount::DeferredPoolBalanceChange,
    block::{Block, Height},
    chain_tip::{mock::MockChainTip, ChainTip},
    parameters::Network::Mainnet,
    serialization::ZcashDeserializeInto,
};
use zebra_node_services::mempool::MempoolTxSubscriber;
use zebra_state::{
    constants::STATE_DATABASE_KIND, state_database_format_version_in_code,
    write_state_database_format_version_to_disk, ChainTipSender, CheckpointVerifiedBlock, Config,
    DiskWriteBatch, FinalizedBlock, ReadRequest, ReadResponse, TransactionLocation, Treestate,
    WriteDisk, ZebraDb,
};
use zebra_test::mock_service::MockService;

use super::{update_finalized_chain_tip, IndexerClient};

tokio::task_local! {
    pub(super) static INCOMPLETE_TIP_READ: Arc<Notify>;
}

#[tokio::test]
async fn finalized_tip_retries_incomplete_body_without_another_notification() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let version = state_database_format_version_in_code();
    let columns = [
        "hash_by_height",
        "height_by_hash",
        "block_header_by_height",
        "tx_by_loc",
        "hash_by_tx_loc",
        "tx_loc_by_hash",
        "utxo_loc_by_transparent_addr_loc",
    ];
    let primary = ZebraDb::new(
        &config,
        STATE_DATABASE_KIND,
        &version,
        &Mainnet,
        true,
        columns.map(str::to_owned),
        false,
    )
    .unwrap();
    write_state_database_format_version_to_disk(&config, &version, &Mainnet).unwrap();
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let finalized = FinalizedBlock::from_checkpoint_verified(
        CheckpointVerifiedBlock::from(block.clone()),
        Treestate::default(),
        DeferredPoolBalanceChange::zero(),
    );
    let mut batch = DiskWriteBatch::new();
    batch.prepare_block_header_and_transaction_data_batch(primary.db(), &finalized);
    // Expose the tip header and index before its body, as secondary WAL catch-up can do.
    batch.zs_delete(
        &primary.cf_handle("tx_by_loc").unwrap(),
        TransactionLocation::min_for_height(Height::MIN),
    );
    primary.write_batch(batch).unwrap();
    primary.db().flush().unwrap();
    let secondary = ZebraDb::new(
        &config,
        STATE_DATABASE_KIND,
        &version,
        &Mainnet,
        true,
        columns.map(str::to_owned),
        true,
    )
    .unwrap();

    let (mut upstream_tip, upstream_sender) = MockChainTip::new();
    upstream_sender.send_best_tip_height(Height::MIN);
    upstream_tip.mark_best_tip_seen();
    // Leave exactly one unseen upstream notification for the subscription.
    upstream_sender.send_best_tip_hash(block.hash());
    let read_state: MockService<ReadRequest, ReadResponse, _, tower::BoxError> =
        MockService::build().for_unit_tests();
    let (mempool_sender, _) = broadcast::channel(1);
    let (server, address) = crate::indexer::server::init(
        "127.0.0.1:0".parse().unwrap(),
        read_state,
        upstream_tip,
        MempoolTxSubscriber::new(mempool_sender),
    )
    .await
    .unwrap();
    let endpoint = tonic::transport::Endpoint::new(format!("http://{address}"))
        .unwrap()
        .connect_timeout(Duration::from_secs(5));
    let client = IndexerClient::connect(endpoint).await.unwrap();
    let (tip_sender, latest_tip, mut tip_change) = ChainTipSender::new(None, &Mainnet);
    let (started_sender, started_receiver) = watch::channel(false);
    let incomplete_tip_read = Arc::new(Notify::new());
    let updater = tokio::spawn(INCOMPLETE_TIP_READ.scope(
        incomplete_tip_read.clone(),
        update_finalized_chain_tip(secondary, client, tip_sender, started_receiver),
    ));
    tokio::time::timeout(Duration::from_secs(30), incomplete_tip_read.notified())
        .await
        .expect("the consumed notification must attempt an incomplete tip read");
    assert_eq!(latest_tip.best_tip_height_and_hash(), None);

    let mut batch = DiskWriteBatch::new();
    batch.prepare_block_header_and_transaction_data_batch(primary.db(), &finalized);
    primary.write_batch(batch).unwrap();
    primary.db().flush().unwrap();
    tokio::time::timeout(Duration::from_secs(30), tip_change.wait_for_tip_change())
        .await
        .expect("the pending update must retry without a second upstream notification")
        .unwrap();
    assert_eq!(
        latest_tip.best_tip_height_and_hash(),
        Some((Height::MIN, block.hash()))
    );

    started_sender.send_replace(true);
    tokio::time::timeout(Duration::from_secs(5), updater)
        .await
        .expect("non-finalized sync handover must stop the updater")
        .unwrap();
    server.abort();
}

/// `try_commit` skips a block that its own catch-up with the primary has just finalized, so it
/// doesn't commit the block, prune the whole non-finalized state, and fail in `update_channels`.
///
/// With mainnet blocks 0-3 (all coinbase-only):
/// 1. the primary finalizes 0 and 1; the secondary catches up and commits block 2;
/// 2. the primary finalizes 2 and 3;
/// 3. block 3 passes `sync()`'s checks (secondary tip still 1), then `try_commit` catches up to 3.
#[tokio::test]
async fn try_commit_skips_block_finalized_by_catch_up() {
    use futures::FutureExt;
    use tower::{Service, ServiceExt};
    use zebra_chain::block::Block;
    use zebra_state::{NonFinalizedState, SemanticallyVerifiedBlock};

    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };

    let blocks: Vec<Arc<Block>> = (0..=3)
        .map(|height| {
            zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
                .get(&height)
                .expect("test vector exists")
                .zcash_deserialize_into()
                .expect("test vector deserializes")
        })
        .collect();
    for block in &blocks[1..] {
        assert_eq!(
            block.transactions.len(),
            1,
            "early mainnet blocks are coinbase-only"
        );
    }

    // The primary: a full state service writing every column family.
    let (mut primary, _read, _tip, _tip_change) =
        zebra_state::init(config.clone(), &Mainnet, Height::MAX, 0).await;
    let mut commit_on_primary = |block: Arc<Block>| {
        let request = zebra_state::Request::CommitCheckpointVerifiedBlock(
            CheckpointVerifiedBlock::from(block),
        );
        let fut = primary
            .ready()
            .now_or_never()
            .expect("ready")
            .expect("ready")
            .call(request);
        async move { fut.await.expect("primary commits checkpoint block") }
    };
    commit_on_primary(blocks[0].clone()).await;
    commit_on_primary(blocks[1].clone()).await;

    // The read-only secondary and a syncer that never needs the gRPC client on this path.
    let (_read_state, db, non_finalized_state_sender) =
        zebra_state::init_read_only(config.clone(), &Mainnet).expect("secondary opens");
    let (chain_tip_sender, _latest, _change) = ChainTipSender::new(None, &Mainnet);
    let (started_sync_sender, _started) = watch::channel(true);
    let channel = tonic::transport::Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
    let mut syncer = super::TrustedChainSync {
        indexer_rpc_client: IndexerClient::new(channel),
        db: db.clone(),
        non_finalized_state: NonFinalizedState::new(&Mainnet),
        chain_tip_sender,
        non_finalized_state_sender,
        started_sync_sender,
    };

    // 1. Secondary catches up to tip 1 and commits block 2 onto it.
    syncer.try_catch_up_with_primary().await;
    assert_eq!(syncer.db.finalized_tip_height(), Some(Height(1)));
    syncer
        .commit(SemanticallyVerifiedBlock::from(blocks[2].clone()))
        .expect("block 2 commits as a new non-finalized chain");
    assert_eq!(
        syncer.non_finalized_state.best_tip().map(|(h, _)| h),
        Some(Height(2))
    );

    // 2. The primary finalizes 2 and 3 before the secondary reads streamed block 3.
    commit_on_primary(blocks[2].clone()).await;
    commit_on_primary(blocks[3].clone()).await;

    // 3. `sync()`'s skip checks pass for block 3: not in our chain, above the finalized tip of 1.
    let block3 = SemanticallyVerifiedBlock::from(blocks[3].clone());
    assert!(!syncer.non_finalized_state.any_chain_contains(&block3.hash));
    assert!(
        syncer
            .db
            .finalized_tip_height()
            .is_none_or(|tip| block3.height > tip),
        "skip check passes: the secondary hasn't caught up yet"
    );

    syncer
        .try_commit(block3)
        .await
        .expect("a block finalized by the catch-up is skipped");
    assert_eq!(
        db.finalized_tip_height(),
        Some(Height(3)),
        "catch-up reached block 3"
    );
}
