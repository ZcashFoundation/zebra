//! Header reads must not mix a stale cached fork with the finalized chain.

use std::{sync::Arc, time::Duration};

use tower::ServiceExt;
use zebra_chain::{
    amount::NonNegative, block::Block, parameters::Network, value_balance::ValueBalance,
};

use crate::{
    arbitrary::Prepare,
    service::{
        finalized_state::FinalizedState, non_finalized_state::NonFinalizedState,
        watch_receiver::WatchReceiver, ReadStateService,
    },
    tests::FakeChainHelper,
    BlockHeader, Config, DiskWriteBatch, ReadRequest, ReadResponse, WriteDisk,
};

#[tokio::test(flavor = "multi_thread")]
async fn header_reads_keep_finalized_and_stale_cached_forks_separate() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let network = Network::Mainnet;
        let finalized = FinalizedState::new_with_debug(
            &Config::ephemeral(),
            &network,
            true,
            #[cfg(feature = "elasticsearch")]
            false,
            false,
        )
        .unwrap();
        finalized.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

        let cached_root: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
        let cached_tip = cached_root.make_fake_child();
        let mut cached = NonFinalizedState::new(&network);
        cached
            .commit_new_chain(cached_root.clone().prepare(), &finalized)
            .unwrap();
        cached
            .commit_block(cached_tip.clone().prepare(), &finalized)
            .unwrap();

        // Finalize a competing fork without updating the cached chain snapshot.
        let finalized_root = cached_root.clone().set_work(50);
        let finalized_child = finalized_root.make_fake_child();
        let finalized_tip = finalized_child.make_fake_child();
        let mut batch = DiskWriteBatch::new();
        for block in [&finalized_root, &finalized_child, &finalized_tip] {
            let height = block.coinbase_height().unwrap();
            let hash = block.hash();
            batch.zs_insert(
                &finalized.db.cf_handle("block_header_by_height").unwrap(),
                height,
                &block.header,
            );
            batch.zs_insert(
                &finalized.db.cf_handle("hash_by_height").unwrap(),
                height,
                hash,
            );
            batch.zs_insert(
                &finalized.db.cf_handle("height_by_hash").unwrap(),
                hash,
                height,
            );
        }
        finalized.db.write_batch(batch).unwrap();
        let (_sender, receiver) = tokio::sync::watch::channel(cached);
        let read_state = ReadStateService::new(&finalized, None, WatchReceiver::new(receiver));

        for (query, block, next_block_hash) in [
            (
                finalized_root.hash().into(),
                &finalized_root,
                Some(finalized_child.hash()),
            ),
            (
                cached_root.coinbase_height().unwrap().into(),
                &cached_root,
                Some(cached_tip.hash()),
            ),
            (cached_tip.hash().into(), &cached_tip, None),
        ] {
            assert_eq!(
                read_state
                    .clone()
                    .oneshot(ReadRequest::BlockHeader(query))
                    .await
                    .unwrap(),
                ReadResponse::BlockHeader(Some(BlockHeader {
                    header: block.header.clone(),
                    hash: block.hash(),
                    height: block.coinbase_height().unwrap(),
                    next_block_hash,
                }))
            );
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn header_reads_keep_cached_successor_of_finalized_tip() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let network = Network::Mainnet;
        let finalized = FinalizedState::new_with_debug(
            &Config::ephemeral(),
            &network,
            true,
            #[cfg(feature = "elasticsearch")]
            false,
            false,
        )
        .unwrap();
        finalized.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

        let root: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
        let child = root.make_fake_child();
        let mut cached = NonFinalizedState::new(&network);
        cached
            .commit_new_chain(root.clone().prepare(), &finalized)
            .unwrap();
        cached
            .commit_block(child.clone().prepare(), &finalized)
            .unwrap();
        let _ = cached.finalize();
        assert!(!cached
            .best_chain()
            .unwrap()
            .contains_block_hash(root.hash()));

        let height = root.coinbase_height().unwrap();
        let hash = root.hash();
        let mut batch = DiskWriteBatch::new();
        batch.zs_insert(
            &finalized.db.cf_handle("block_header_by_height").unwrap(),
            height,
            &root.header,
        );
        batch.zs_insert(
            &finalized.db.cf_handle("hash_by_height").unwrap(),
            height,
            hash,
        );
        batch.zs_insert(
            &finalized.db.cf_handle("height_by_hash").unwrap(),
            hash,
            height,
        );
        finalized.db.write_batch(batch).unwrap();
        let (_sender, receiver) = tokio::sync::watch::channel(cached);
        let read_state = ReadStateService::new(&finalized, None, WatchReceiver::new(receiver));

        for query in [hash.into(), height.into()] {
            assert_eq!(
                read_state
                    .clone()
                    .oneshot(ReadRequest::BlockHeader(query))
                    .await
                    .unwrap(),
                ReadResponse::BlockHeader(Some(BlockHeader {
                    header: root.header.clone(),
                    hash,
                    height,
                    next_block_hash: Some(child.hash()),
                }))
            );
        }
    })
    .await
    .unwrap();
}
