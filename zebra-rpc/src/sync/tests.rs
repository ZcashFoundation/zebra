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
