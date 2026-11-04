//! Regression tests for secondary finalized-tip publication and non-finalized commits.

use std::{sync::Arc, time::Duration};

use tokio::sync::{broadcast, watch, Notify};
use tonic::transport::Endpoint;
use tower::{Service, ServiceExt};
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
    DiskWriteBatch, FinalizedBlock, NonFinalizedState, ReadRequest, ReadResponse, Request,
    SemanticallyVerifiedBlock, TransactionLocation, Treestate, WriteDisk, ZebraDb,
};
use zebra_test::mock_service::MockService;

use super::{update_finalized_chain_tip, IndexerClient, TrustedChainSync};

/// Columns read by these tests, so the databases skip unrelated column families.
const COLUMNS: [&str; 7] = [
    "hash_by_height",
    "height_by_hash",
    "block_header_by_height",
    "tx_by_loc",
    "hash_by_tx_loc",
    "tx_loc_by_hash",
    "utxo_loc_by_transparent_addr_loc",
];

/// Opens a primary, or a secondary of it, with [`COLUMNS`].
fn open(config: &Config, secondary: bool) -> ZebraDb {
    ZebraDb::new(
        config,
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        &Mainnet,
        true,
        COLUMNS.map(str::to_owned),
        secondary,
    )
    .unwrap()
}

/// Writes `block`'s header and transactions to `primary` and flushes them for secondaries.
fn write_block(primary: &ZebraDb, block: Arc<Block>) {
    let finalized = FinalizedBlock::from_checkpoint_verified(
        CheckpointVerifiedBlock::from(block),
        Treestate::default(),
        DeferredPoolBalanceChange::zero(),
    );
    let mut batch = DiskWriteBatch::new();
    batch.prepare_block_header_and_transaction_data_batch(primary.db(), &finalized);
    primary.write_batch(batch).unwrap();
    primary.db().flush().unwrap();
}

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
    let primary = open(&config, false);
    write_state_database_format_version_to_disk(
        &config,
        &state_database_format_version_in_code(),
        &Mainnet,
    )
    .unwrap();
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
    let secondary = open(&config, true);

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

    write_block(&primary, block.clone());
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

/// A streamed block that the secondary's catch-up finalizes is skipped, not committed onto a
/// missing parent.
#[tokio::test]
async fn commit_skips_a_block_finalized_by_catch_up() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let primary = open(&config, false);
    write_state_database_format_version_to_disk(
        &config,
        &state_database_format_version_in_code(),
        &Mainnet,
    )
    .unwrap();
    let genesis: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    write_block(&primary, genesis);
    let secondary = open(&config, true);
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    write_block(&primary, block.clone());
    assert_eq!(secondary.finalized_tip_height(), Some(Height::MIN));

    let (chain_tip_sender, _, _) = ChainTipSender::new(None, &Mainnet);
    let mut syncer = TrustedChainSync {
        // Never connected: the block is skipped before any request.
        indexer_rpc_client: IndexerClient::new(
            Endpoint::from_static("http://127.0.0.1:1").connect_lazy(),
        ),
        db: secondary.clone(),
        non_finalized_state: NonFinalizedState::new(&Mainnet),
        chain_tip_sender,
        non_finalized_state_sender: watch::channel(NonFinalizedState::new(&Mainnet)).0,
        started_sync_sender: watch::channel(true).0,
    };
    let hash = block.hash();

    assert_eq!(
        syncer
            .try_commit(SemanticallyVerifiedBlock::with_hash(block, hash))
            .await,
        Ok(())
    );
    assert_eq!(secondary.finalized_tip_hash(), hash);
    assert!(syncer.non_finalized_state.best_chain().is_none());
}

/// Catch-up drops a higher-work conflicting fork and publishes matching suffixes, even when the
/// streamed block is already finalized. Empty suffixes fall back to the database tip.
#[tokio::test]
async fn catch_up_reconciles_forks_before_skipping_finalized_blocks() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let (mut primary, read_state, _, _) =
        zebra_state::init(config.clone(), &Mainnet, Height::MAX, 0).await;
    let genesis: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    primary
        .ready()
        .await
        .unwrap()
        .call(Request::CommitCheckpointVerifiedBlock(genesis.into()))
        .await
        .unwrap();
    let primary_db = read_state.db();
    primary_db.db().flush().unwrap();
    let secondary = open(&config, true);
    let blocks: [Arc<Block>; 3] = [
        &*zebra_test::vectors::BLOCK_MAINNET_1_BYTES,
        &*zebra_test::vectors::BLOCK_MAINNET_2_BYTES,
        &*zebra_test::vectors::BLOCK_MAINNET_3_BYTES,
    ]
    .map(|bytes| bytes.as_slice().zcash_deserialize_into().unwrap());
    let prepare =
        |block: &Arc<Block>| SemanticallyVerifiedBlock::with_hash(block.clone(), block.hash());
    let (chain_tip_sender, latest, _) = ChainTipSender::new(None, &Mainnet);
    let (state_sender, snapshot) = watch::channel(NonFinalizedState::new(&Mainnet));
    let mut syncer = TrustedChainSync {
        indexer_rpc_client: IndexerClient::new(
            Endpoint::from_static("http://127.0.0.1:1").connect_lazy(),
        ),
        db: secondary,
        non_finalized_state: NonFinalizedState::new(&Mainnet),
        chain_tip_sender,
        non_finalized_state_sender: state_sender,
        started_sync_sender: watch::channel(true).0,
    };
    for block in &blocks[..2] {
        syncer.commit(prepare(block)).unwrap();
    }
    let mut parent = None;
    for block in &blocks {
        let mut fork = block.clone();
        let header = Arc::make_mut(&mut Arc::make_mut(&mut fork).header);
        header.nonce[0] ^= 1;
        if let Some(parent) = parent {
            header.previous_block_hash = parent;
        }
        parent = Some(fork.hash());
        syncer.commit(prepare(&fork)).unwrap();
    }
    let stale_tip = parent.unwrap();
    assert_eq!(latest.best_tip_hash(), Some(stale_tip));
    assert_eq!(snapshot.borrow().chain_count(), 2);
    let canonical = syncer
        .non_finalized_state
        .chain_iter()
        .find(|chain| chain.contains_block_hash(blocks[1].hash()))
        .unwrap()
        .clone();

    for block in &blocks[..2] {
        primary
            .ready()
            .await
            .unwrap()
            .call(Request::CommitCheckpointVerifiedBlock(block.clone().into()))
            .await
            .unwrap();
        primary_db.db().flush().unwrap();
        tokio::time::timeout(Duration::from_secs(30), syncer.try_commit(prepare(block)))
            .await
            .unwrap()
            .unwrap();
        assert!(!snapshot.borrow().any_chain_contains(&stale_tip));
        assert_eq!(latest.best_tip_hash(), Some(blocks[1].hash()));
        if block.hash() == blocks[0].hash() {
            assert_eq!(snapshot.borrow().chain_count(), 1);
            assert!(Arc::ptr_eq(
                snapshot.borrow().best_chain().unwrap(),
                &canonical
            ));
            assert_eq!(snapshot.borrow().root_height(), Some(Height(1)));
        } else {
            assert!(snapshot.borrow().best_chain().is_none());
        }
    }
    syncer.try_commit(prepare(&blocks[2])).await.unwrap();
    assert_eq!(latest.best_tip_hash(), Some(blocks[2].hash()));
    assert_eq!(snapshot.borrow().chain_count(), 1);
    // A chain rooted directly above the database tip survives catch-up without a copy.
    let direct_child = snapshot.borrow().best_chain().unwrap().clone();
    tokio::time::timeout(Duration::from_secs(30), syncer.try_catch_up_with_primary())
        .await
        .unwrap();
    assert!(Arc::ptr_eq(
        syncer.non_finalized_state.best_chain().unwrap(),
        &direct_child
    ));
    assert_eq!(syncer.non_finalized_state.root_height(), Some(Height(3)));
}

/// A torn finalized body must not clear a follower tip when catch-up discards its last fork.
#[tokio::test]
async fn catch_up_keeps_tip_until_finalized_body_is_readable() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let (mut primary, read_state, _, _) =
        zebra_state::init(config.clone(), &Mainnet, Height::MAX, 0).await;
    let genesis: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    primary
        .ready()
        .await
        .unwrap()
        .call(Request::CommitCheckpointVerifiedBlock(genesis.into()))
        .await
        .unwrap();
    let primary_db = read_state.db();
    primary_db.db().flush().unwrap();
    let secondary = open(&config, true);
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let mut fork = block.clone();
    Arc::make_mut(&mut Arc::make_mut(&mut fork).header).nonce[0] ^= 1;
    let (chain_tip_sender, latest, _) = ChainTipSender::new(None, &Mainnet);
    let (state_sender, snapshot) = watch::channel(NonFinalizedState::new(&Mainnet));
    let mut syncer = TrustedChainSync {
        indexer_rpc_client: IndexerClient::new(
            Endpoint::from_static("http://127.0.0.1:1").connect_lazy(),
        ),
        db: secondary,
        non_finalized_state: NonFinalizedState::new(&Mainnet),
        chain_tip_sender,
        non_finalized_state_sender: state_sender,
        started_sync_sender: watch::channel(true).0,
    };
    syncer
        .commit(SemanticallyVerifiedBlock::with_hash(
            fork.clone(),
            fork.hash(),
        ))
        .unwrap();
    primary
        .ready()
        .await
        .unwrap()
        .call(Request::CommitCheckpointVerifiedBlock(block.clone().into()))
        .await
        .unwrap();
    let mut batch = DiskWriteBatch::new();
    batch.zs_delete(
        &primary_db.cf_handle("tx_by_loc").unwrap(),
        TransactionLocation::min_for_height(Height(1)),
    );
    primary_db.write_batch(batch).unwrap();
    primary_db.db().flush().unwrap();

    for readable in [false, true] {
        if readable {
            write_block(primary_db, block.clone());
        }
        tokio::time::timeout(
            Duration::from_secs(30),
            syncer.try_commit(SemanticallyVerifiedBlock::with_hash(
                block.clone(),
                block.hash(),
            )),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(snapshot.borrow().best_chain().is_none());
        assert_eq!(
            latest.best_tip_hash(),
            Some(if readable { block.hash() } else { fork.hash() })
        );
    }
}
