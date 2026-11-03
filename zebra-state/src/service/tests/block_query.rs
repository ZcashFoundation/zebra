//! `ReadRequest::BlockQuery` must read each requested part from one chain snapshot, and only
//! the rows those parts need.

use std::{sync::Arc, time::Duration};

use tower::ServiceExt;
use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Block, Height, SerializedBlock},
    block_info::BlockInfo,
    parameters::Network,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::Transaction,
    transparent::{Input, OutPoint, Output, Script},
    value_balance::ValueBalance,
};

use crate::{
    arbitrary::Prepare,
    service::{
        arbitrary::populated_state_with_debug,
        finalized_state::FinalizedState,
        non_finalized_state::{Chain, NonFinalizedState},
        read::{self, block_query::HISTORY_CAPABILITIES_READS},
        watch_receiver::WatchReceiver,
        ReadStateService,
    },
    tests::FakeChainHelper,
    BlockQueryFields, BlockReadError, CheckpointVerifiedBlock, Config, DiskWriteBatch,
    HashOrHeight, QueriedBlock, ReadRequest, ReadResponse, StateInitError, TransactionLocation,
    WriteDisk,
};

/// The parts that need the block body.
const BODY: BlockQueryFields = BlockQueryFields::RAW_BLOCK
    .union(BlockQueryFields::TRANSACTIONS)
    .union(BlockQueryFields::SPENT_OUTPUTS)
    .union(BlockQueryFields::AUTH_DATA_ROOT);

/// The parts that need the deserialized transactions.
const TRANSACTIONS: BlockQueryFields = BlockQueryFields::TRANSACTIONS
    .union(BlockQueryFields::SPENT_OUTPUTS)
    .union(BlockQueryFields::AUTH_DATA_ROOT);

fn mainnet_blocks() -> Vec<Arc<Block>> {
    zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|bytes| bytes.zcash_deserialize_into().unwrap())
        .collect()
}

/// Returns an ephemeral Mainnet finalized state with `blocks` committed in order.
fn finalized_state(blocks: &[Arc<Block>]) -> FinalizedState {
    let mut state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        &Network::Mainnet,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .unwrap();
    for block in blocks {
        state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(block.clone()).into(),
                None,
                "test",
            )
            .unwrap();
    }

    state
}

/// Sends one `BlockQuery` through `read_state`.
async fn query(
    read_state: &ReadStateService,
    hash_or_height: HashOrHeight,
    fields: BlockQueryFields,
) -> Result<Option<QueriedBlock>, BlockReadError> {
    let request = ReadRequest::BlockQuery {
        hash_or_height,
        fields,
    };
    match read_state.clone().oneshot(request).await {
        Ok(ReadResponse::BlockQuery(queried)) => Ok(queried),
        Ok(response) => panic!("unexpected response {response:?}"),
        Err(error) => Err(error
            .downcast_ref::<BlockReadError>()
            .cloned()
            .unwrap_or_else(|| panic!("unexpected error {error}"))),
    }
}

/// Returns `full` with every part that `fields` does not request set to `None`.
fn requested(full: &QueriedBlock, fields: BlockQueryFields) -> QueriedBlock {
    let has = |flag| fields.contains(flag);
    QueriedBlock {
        hash: full.hash,
        height: full.height,
        header: full
            .header
            .clone()
            .filter(|_| has(BlockQueryFields::HEADER)),
        depth: full.depth.filter(|_| has(BlockQueryFields::DEPTH)),
        next_block_hash: full
            .next_block_hash
            .filter(|_| has(BlockQueryFields::NEXT_BLOCK_HASH)),
        transaction_count: full
            .transaction_count
            .filter(|_| has(BlockQueryFields::TRANSACTION_COUNT)),
        transaction_ids: full
            .transaction_ids
            .clone()
            .filter(|_| has(BlockQueryFields::TRANSACTION_IDS)),
        block_info: full
            .block_info
            .clone()
            .filter(|_| has(BlockQueryFields::BLOCK_INFO)),
        previous_block_info: full
            .previous_block_info
            .clone()
            .filter(|_| has(BlockQueryFields::PREVIOUS_BLOCK_INFO)),
        sapling_tree: full
            .sapling_tree
            .clone()
            .filter(|_| has(BlockQueryFields::SAPLING_TREE)),
        orchard_tree: full
            .orchard_tree
            .clone()
            .filter(|_| has(BlockQueryFields::ORCHARD_TREE)),
        ironwood_tree: full
            .ironwood_tree
            .clone()
            .filter(|_| has(BlockQueryFields::IRONWOOD_TREE)),
        raw_block: full
            .raw_block
            .clone()
            .filter(|_| has(BlockQueryFields::RAW_BLOCK)),
        block: full
            .block
            .clone()
            .filter(|_| fields.intersects(TRANSACTIONS)),
        size: full.size.filter(|_| fields.intersects(BODY)),
        spent_outputs: full
            .spent_outputs
            .clone()
            .filter(|_| has(BlockQueryFields::SPENT_OUTPUTS)),
        auth_data_root: full
            .auth_data_root
            .filter(|_| has(BlockQueryFields::AUTH_DATA_ROOT)),
    }
}

/// Returns what `BlockQuery` with every field must return for `block`, from the requests that
/// each read one part.
async fn expected_from_single_part_requests(
    read_state: &ReadStateService,
    block: &Arc<Block>,
) -> QueriedBlock {
    let hash = block.hash();
    let height = block.coinbase_height().unwrap();
    let request = |request| read_state.clone().oneshot(request);

    let ReadResponse::BlockHeader(Some(header)) = request(ReadRequest::BlockHeader(hash.into()))
        .await
        .unwrap()
    else {
        panic!("the block has a header");
    };
    let ReadResponse::Depth(depth) = request(ReadRequest::Depth(hash)).await.unwrap() else {
        panic!("unexpected response");
    };
    let ReadResponse::TransactionIdsForBlock(transaction_ids) =
        request(ReadRequest::TransactionIdsForBlock(hash.into()))
            .await
            .unwrap()
    else {
        panic!("unexpected response");
    };
    let ReadResponse::BlockInfo(block_info) =
        request(ReadRequest::BlockInfo(hash.into())).await.unwrap()
    else {
        panic!("unexpected response");
    };
    let previous_block_info = match height.previous() {
        Ok(parent) => {
            let ReadResponse::BlockInfo(block_info) =
                request(ReadRequest::BlockInfo(parent.into()))
                    .await
                    .unwrap()
            else {
                panic!("unexpected response");
            };
            block_info
        }
        Err(_) => None,
    };
    let ReadResponse::SaplingTree(sapling_tree) = request(ReadRequest::SaplingTree(hash.into()))
        .await
        .unwrap()
    else {
        panic!("unexpected response");
    };
    let ReadResponse::OrchardTree(orchard_tree) = request(ReadRequest::OrchardTree(hash.into()))
        .await
        .unwrap()
    else {
        panic!("unexpected response");
    };
    let ReadResponse::IronwoodTree(ironwood_tree) = request(ReadRequest::IronwoodTree(hash.into()))
        .await
        .unwrap()
    else {
        panic!("unexpected response");
    };
    let ReadResponse::Block(Some(stored_block)) =
        request(ReadRequest::Block(hash.into())).await.unwrap()
    else {
        panic!("the block has a body");
    };
    let ReadResponse::BlockAndSize(Some((_, size))) =
        request(ReadRequest::BlockAndSize(hash.into()))
            .await
            .unwrap()
    else {
        panic!("the block has a body");
    };
    let ReadResponse::SpentOutputs(spent_outputs) = request(ReadRequest::SpentOutputs(hash.into()))
        .await
        .unwrap()
    else {
        panic!("unexpected response");
    };
    assert_eq!(&stored_block, block);

    QueriedBlock {
        hash,
        height,
        header: Some(header.header),
        depth,
        next_block_hash: header.next_block_hash,
        transaction_count: transaction_ids.as_ref().map(|ids| ids.len()),
        transaction_ids,
        block_info,
        previous_block_info,
        sapling_tree,
        orchard_tree,
        ironwood_tree,
        raw_block: Some(SerializedBlock::from(block.as_ref())),
        block: Some(stored_block),
        size: Some(size),
        spent_outputs,
        auth_data_root: Some(block.auth_data_root()),
    }
}

/// Every part of a finalized block equals the answer of the request that reads only that part,
/// by hash and by height, and each part is returned only when it is requested.
#[tokio::test(flavor = "multi_thread")]
async fn block_query_matches_the_single_part_requests() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(120), async {
        let blocks = mainnet_blocks();
        let (_state, read_state, _, _) =
            populated_state_with_debug(blocks.clone(), &Network::Mainnet, true).await;

        for block in &blocks {
            let expected = expected_from_single_part_requests(&read_state, block).await;
            assert_eq!(expected.transaction_count, Some(block.transactions.len()));

            for hash_or_height in [expected.height.into(), expected.hash.into()] {
                let full = query(&read_state, hash_or_height, BlockQueryFields::all())
                    .await
                    .unwrap();
                assert_eq!(full.as_ref(), Some(&expected), "{hash_or_height:?}");
                assert_eq!(
                    read::block_query(
                        None,
                        read_state.db(),
                        hash_or_height,
                        BlockQueryFields::all()
                    ),
                    Ok(Some(expected.clone())),
                );

                for fields in BlockQueryFields::all()
                    .iter()
                    .chain([BlockQueryFields::empty(), BlockQueryFields::all() - BODY])
                {
                    assert_eq!(
                        query(&read_state, hash_or_height, fields).await,
                        Ok(Some(requested(&expected, fields))),
                        "{hash_or_height:?} {fields:?}",
                    );
                }
            }
        }

        let tip = blocks.last().unwrap().coinbase_height().unwrap();
        for unknown in [tip.next().unwrap().into(), block::Hash([0xff; 32]).into()] {
            assert_eq!(
                query(&read_state, unknown, BlockQueryFields::all()).await,
                Ok(None)
            );
        }
    })
    .await
    .unwrap();
}

/// With a non-finalized chain above the finalized tip, depths count from the chain's tip and the
/// finalized tip's next block is the chain's root, whichever side the block is read from.
#[tokio::test(flavor = "multi_thread")]
async fn block_query_counts_depth_and_next_hash_from_the_sampled_chain() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(120), async {
        let network = Network::Mainnet;
        let finalized_blocks = mainnet_blocks();
        let finalized = finalized_state(&finalized_blocks);

        let mut cached_blocks = vec![finalized_blocks.last().unwrap().make_fake_child()];
        for _ in 0..2 {
            let child = cached_blocks.last().unwrap().make_fake_child();
            cached_blocks.push(child);
        }
        let mut cached = NonFinalizedState::new(&network);
        cached
            .commit_new_chain(cached_blocks[0].clone().prepare(), &finalized)
            .unwrap();
        for block in &cached_blocks[1..] {
            cached
                .commit_block(block.clone().prepare(), &finalized)
                .unwrap();
        }
        let chain = cached.best_chain().cloned();
        let chain_tip = cached_blocks.last().unwrap().coinbase_height().unwrap();
        let (_sender, receiver) = tokio::sync::watch::channel(cached);
        let read_state = ReadStateService::new(&finalized, None, WatchReceiver::new(receiver));

        let blocks: Vec<_> = finalized_blocks.iter().chain(&cached_blocks).collect();
        let mut previous: Option<QueriedBlock> = None;
        for (index, block) in blocks.iter().enumerate() {
            let hash = block.hash();
            let height = block.coinbase_height().unwrap();
            let next_block_hash = blocks.get(index + 1).map(|next| next.hash());

            let queried = read::block_query(
                chain.clone(),
                &finalized.db,
                hash.into(),
                BlockQueryFields::all(),
            )
            .unwrap()
            .unwrap();

            assert_eq!(queried.height, height);
            assert_eq!(queried.depth, Some(chain_tip.0 - height.0), "{height:?}");
            assert_eq!(
                queried.depth,
                read::depth(chain.clone(), &finalized.db, hash),
                "{height:?}"
            );
            assert_eq!(queried.next_block_hash, next_block_hash, "{height:?}");
            assert_eq!(queried.header.as_ref(), Some(&block.header));
            assert_eq!(
                queried.transaction_ids.as_deref(),
                Some(
                    block
                        .transactions
                        .iter()
                        .map(|transaction| transaction.hash())
                        .collect::<Vec<_>>()
                        .as_slice()
                )
            );
            assert_eq!(queried.transaction_count, Some(block.transactions.len()));
            assert_eq!(queried.block.as_ref(), Some(*block));
            assert_eq!(
                queried.raw_block,
                Some(SerializedBlock::from(block.as_ref()))
            );
            assert_eq!(queried.size, Some(block.zcash_serialized_size()));
            assert_eq!(
                queried.block_info,
                read::block_info(chain.clone(), &finalized.db, hash.into())
            );
            assert!(queried.block_info.is_some(), "{height:?}");
            assert_eq!(
                queried.sapling_tree,
                read::sapling_tree(chain.clone(), &finalized.db, hash.into())
            );
            assert_eq!(
                queried.orchard_tree,
                read::orchard_tree(chain.clone(), &finalized.db, hash.into())
            );
            assert_eq!(
                queried.ironwood_tree,
                read::ironwood_tree(chain.clone(), &finalized.db, hash.into())
            );
            // The chain's root reads its parent's `BlockInfo` from the finalized state.
            assert_eq!(
                queried.previous_block_info,
                previous.and_then(|previous| previous.block_info),
                "{height:?}"
            );

            assert_eq!(
                read::block_query(
                    chain.clone(),
                    &finalized.db,
                    height.into(),
                    BlockQueryFields::all()
                ),
                Ok(Some(queried.clone()))
            );
            for hash_or_height in [hash.into(), height.into()] {
                assert_eq!(
                    query(&read_state, hash_or_height, BlockQueryFields::all()).await,
                    Ok(Some(queried.clone()))
                );
            }

            previous = Some(queried);
        }
    })
    .await
    .unwrap();
}

/// A stale cached chain can be a fork of the finalized chain at the same heights. Depths and
/// next hashes must not combine its blocks with finalized rows, as for `BlockHeader` in
/// `header.rs`, and a finalized block above the stale chain's tip counts its depth from the
/// finalized tip.
#[tokio::test(flavor = "multi_thread")]
async fn block_query_keeps_finalized_and_stale_cached_forks_separate() {
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

        // Finalize a longer competing fork without updating the cached chain snapshot.
        let finalized_root = cached_root.clone().set_work(50);
        let finalized_child = finalized_root.make_fake_child();
        let finalized_grandchild = finalized_child.make_fake_child();
        let finalized_tip = finalized_grandchild.make_fake_child();
        let mut batch = DiskWriteBatch::new();
        for block in [
            &finalized_root,
            &finalized_child,
            &finalized_grandchild,
            &finalized_tip,
        ] {
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

        let fields =
            BlockQueryFields::HEADER | BlockQueryFields::DEPTH | BlockQueryFields::NEXT_BLOCK_HASH;
        for (hash_or_height, block, next_block_hash, depth) in [
            (
                finalized_root.hash().into(),
                &finalized_root,
                Some(finalized_child.hash()),
                1,
            ),
            (
                cached_root.coinbase_height().unwrap().into(),
                &cached_root,
                Some(cached_tip.hash()),
                1,
            ),
            (cached_tip.hash().into(), &cached_tip, None, 0),
            (
                finalized_grandchild.hash().into(),
                &finalized_grandchild,
                Some(finalized_tip.hash()),
                1,
            ),
            (finalized_tip.hash().into(), &finalized_tip, None, 0),
        ] {
            let expected = QueriedBlock {
                hash: block.hash(),
                height: block.coinbase_height().unwrap(),
                header: Some(block.header.clone()),
                depth: Some(depth),
                next_block_hash,
                transaction_count: None,
                transaction_ids: None,
                block_info: None,
                previous_block_info: None,
                sapling_tree: None,
                orchard_tree: None,
                ironwood_tree: None,
                raw_block: None,
                block: None,
                size: None,
                spent_outputs: None,
                auth_data_root: None,
            };
            assert_eq!(
                query(&read_state, hash_or_height, fields).await,
                Ok(Some(expected)),
                "{hash_or_height:?}"
            );
        }
    })
    .await
    .unwrap();
}

/// Pruning deletes only bodies, so every part except those that need the body is still served,
/// and the parts that need it fail the whole request.
#[tokio::test(flavor = "multi_thread")]
async fn block_query_serves_index_parts_of_pruned_bodies() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let blocks = mainnet_blocks();
        let (_state, read_state, _, _) =
            populated_state_with_debug(blocks.clone(), &Network::Mainnet, true).await;
        let height = Height(1);
        let queries = [height.into(), blocks[1].hash().into()];
        let mut full = Vec::new();
        for hash_or_height in queries {
            full.push(
                query(&read_state, hash_or_height, BlockQueryFields::all())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }

        // Prune the body at height 1, which leaves the retained floor at height 2.
        let db = read_state.db();
        let mut batch = DiskWriteBatch::new();
        batch.zs_delete_range(
            &db.cf_handle("tx_by_loc").unwrap(),
            TransactionLocation::min_for_height(height),
            TransactionLocation::min_for_height(height.next().unwrap()),
        );
        db.write_batch(batch).unwrap();

        let index = BlockQueryFields::all() - BODY;
        for (hash_or_height, full) in queries.into_iter().zip(&full) {
            assert_eq!(
                query(&read_state, hash_or_height, index).await,
                Ok(Some(requested(full, index)))
            );
            for body in BODY.iter() {
                for fields in [body, body | index] {
                    assert_eq!(
                        query(&read_state, hash_or_height, fields).await,
                        Err(BlockReadError::BlockBodyUnavailable { height }),
                        "{fields:?}"
                    );
                }
            }
        }

        assert!(
            query(&read_state, Height(2).into(), BlockQueryFields::all())
                .await
                .unwrap()
                .is_some()
        );
    })
    .await
    .unwrap();
}

/// A missing header of a known block is corrupt for header reads, but parts that come from the
/// transaction index are still served.
#[tokio::test(flavor = "multi_thread")]
async fn block_query_reads_headers_only_for_header_parts() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let blocks = mainnet_blocks();
        let (_state, read_state, _, _) =
            populated_state_with_debug(blocks.clone(), &Network::Mainnet, true).await;
        let height = Height(1);
        let queries = [height.into(), blocks[1].hash().into()];

        let db = read_state.db();
        let mut batch = DiskWriteBatch::new();
        batch.zs_delete(&db.cf_handle("block_header_by_height").unwrap(), height);
        db.write_batch(batch).unwrap();

        let without_header = BlockQueryFields::all() - BODY - BlockQueryFields::HEADER;
        for hash_or_height in queries {
            let queried = query(&read_state, hash_or_height, without_header)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                queried.transaction_ids.as_deref(),
                Some([blocks[1].transactions[0].hash()].as_slice())
            );
            assert_eq!(queried.transaction_count, Some(1));
            for fields in [
                BlockQueryFields::HEADER,
                BlockQueryFields::HEADER | without_header,
                BlockQueryFields::RAW_BLOCK,
            ] {
                assert_eq!(
                    query(&read_state, hash_or_height, fields).await,
                    Err(BlockReadError::CorruptBlock { height }),
                    "{fields:?}"
                );
            }
        }
    })
    .await
    .unwrap();
}

/// Returns `block` with its coinbase outputs replaced by `outputs`.
fn with_coinbase_outputs(block: Arc<Block>, outputs: Vec<Output>) -> Arc<Block> {
    let mut block = Block::clone(&block);
    let coinbase = Transaction::clone(&block.transactions[0]).with_transparent_outputs(outputs);
    block.transactions[0] = Arc::new(coinbase);
    Arc::new(block)
}

/// Returns a finalized state with genesis, a parent block whose coinbase has two outputs, and a
/// spending block with one transaction per output, and returns the state and the spending block.
fn spending_chain() -> (FinalizedState, Arc<Block>) {
    let genesis: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let output = || Output::new(Amount::try_from(1).unwrap(), Script::new(&[0x76; 25]));
    let parent = with_coinbase_outputs(genesis.make_fake_child(), vec![output(), output()]);

    let mut spender = Block::clone(&with_coinbase_outputs(
        parent.make_fake_child(),
        vec![output()],
    ));
    for index in 0..2 {
        let input = Input::PrevOut {
            outpoint: OutPoint {
                hash: parent.transactions[0].hash(),
                index,
            },
            unlock_script: Script::new(&[]),
            sequence: u32::MAX,
        };
        let spend = Transaction::clone(&genesis.transactions[0])
            .with_transparent_inputs(vec![input])
            .with_transparent_outputs(Vec::new());
        spender.transactions.push(Arc::new(spend));
    }
    let spender = Arc::new(spender);

    (
        finalized_state(&[genesis, parent, spender.clone()]),
        spender,
    )
}

/// Spent outputs are resolved from the block the query read, and a pruned parent fails the
/// request without affecting the spending block's own body.
#[test]
fn block_query_resolves_spent_outputs_and_rejects_pruned_parents() {
    let _init_guard = zebra_test::init();
    let (state, spender) = spending_chain();
    let height = spender.coinbase_height().unwrap();

    let queried = read::block_query(
        None,
        &state.db,
        height.into(),
        BlockQueryFields::SPENT_OUTPUTS,
    )
    .unwrap()
    .unwrap();
    let spent_outputs = queried.spent_outputs.unwrap();
    assert_eq!(spent_outputs.len(), 2);
    assert_eq!(
        Some(spent_outputs),
        read::spent_outputs_for_block(None::<Arc<Chain>>, &state.db, height.into()).unwrap()
    );
    assert_eq!(queried.block, Some(spender.clone()));

    let parent = Height(1);
    let mut batch = DiskWriteBatch::new();
    batch.zs_delete_range(
        &state.db.cf_handle("tx_by_loc").unwrap(),
        TransactionLocation::min_for_height(parent),
        TransactionLocation::min_for_height(height),
    );
    state.db.write_batch(batch).unwrap();

    assert_eq!(
        read::block_query(
            None,
            &state.db,
            height.into(),
            BlockQueryFields::SPENT_OUTPUTS
        ),
        Err(BlockReadError::BlockBodyUnavailable { height: parent })
    );
    assert_eq!(
        read::block_query(
            None,
            &state.db,
            height.into(),
            BlockQueryFields::TRANSACTIONS | BlockQueryFields::AUTH_DATA_ROOT
        )
        .unwrap()
        .unwrap()
        .block,
        Some(spender)
    );
}

/// Spent outputs read each parent transaction's body without its block header, which only
/// carries the block time that a spent output does not need.
#[test]
fn spent_outputs_read_parent_bodies_without_their_headers() {
    let _init_guard = zebra_test::init();
    let (state, spender) = spending_chain();
    let height = spender.coinbase_height().unwrap();
    let spent_outputs =
        || read::spent_outputs_for_block(None::<Arc<Chain>>, &state.db, height.into());
    let expected = spent_outputs().unwrap();
    assert_eq!(expected.as_ref().map(|outputs| outputs.len()), Some(2));

    let mut batch = DiskWriteBatch::new();
    batch.zs_delete(
        &state.db.cf_handle("block_header_by_height").unwrap(),
        Height(1),
    );
    state.db.write_batch(batch).unwrap();

    assert_eq!(spent_outputs(), Ok(expected.clone()));
    assert_eq!(
        read::block_query(
            None,
            &state.db,
            height.into(),
            BlockQueryFields::SPENT_OUTPUTS
        )
        .map(|queried| queried.and_then(|queried| queried.spent_outputs)),
        Ok(expected)
    );
}

/// Spent outputs of non-finalized chain blocks come from the chain's created outputs and the
/// finalized UTXO set, which keeps the outputs that only the chain spends, so they don't need the
/// finalized parents' bodies.
#[test]
fn block_query_resolves_spent_outputs_of_chain_blocks_from_the_utxo_set() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let genesis: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let output = || Output::new(Amount::try_from(1).unwrap(), Script::new(&[0x76; 25]));
    let spend = |outpoint, outputs| {
        let input = Input::PrevOut {
            outpoint,
            unlock_script: Script::new(&[]),
            sequence: u32::MAX,
        };
        Arc::new(
            Transaction::clone(&genesis.transactions[0])
                .with_transparent_inputs(vec![input])
                .with_transparent_outputs(outputs),
        )
    };

    // Coinbase outputs are immature in the next blocks, so the chain spends the outputs of a
    // non-coinbase transaction.
    let funding = Arc::new(
        Transaction::clone(&genesis.transactions[0])
            .with_transparent_inputs(Vec::new())
            .with_transparent_outputs(vec![output(), output()]),
    );
    let funding_output = |index| OutPoint {
        hash: funding.hash(),
        index,
    };
    let mut parent = Block::clone(&genesis.make_fake_child());
    parent.transactions.push(funding.clone());
    let parent = Arc::new(parent);
    let retained = parent.make_fake_child();
    let finalized = finalized_state(&[genesis.clone(), parent.clone(), retained.clone()]);

    let mut root = Block::clone(&retained.make_fake_child());
    root.transactions
        .push(spend(funding_output(0), vec![output()]));
    root.transactions.push(spend(funding_output(1), Vec::new()));
    let root = Arc::new(root);
    let mut child = Block::clone(&root.make_fake_child());
    let chain_output = OutPoint {
        hash: root.transactions[1].hash(),
        index: 0,
    };
    child.transactions.push(spend(chain_output, Vec::new()));
    let child = Arc::new(child);

    let mut cached = NonFinalizedState::new(&network);
    cached
        .commit_new_chain(root.clone().prepare(), &finalized)
        .unwrap();
    cached
        .commit_block(child.clone().prepare(), &finalized)
        .unwrap();
    let chain = cached.best_chain().cloned();

    let parent_height = parent.coinbase_height().unwrap();
    let root_height = root.coinbase_height().unwrap();
    let check_spent_outputs = || {
        for (block, mut outpoints, height) in [
            (
                &root,
                vec![funding_output(0), funding_output(1)],
                parent_height,
            ),
            (&child, vec![chain_output], root_height),
        ] {
            let spent_outputs = read::block_query(
                chain.clone(),
                &finalized.db,
                block.hash().into(),
                BlockQueryFields::SPENT_OUTPUTS,
            )
            .unwrap()
            .unwrap()
            .spent_outputs
            .unwrap();
            assert_eq!(
                Some(&spent_outputs),
                read::spent_outputs_for_block(chain.clone(), &finalized.db, block.hash().into())
                    .unwrap()
                    .as_ref()
            );

            let mut spent: Vec<_> = spent_outputs.keys().copied().collect();
            spent.sort_by_key(|outpoint| outpoint.index);
            outpoints.sort_by_key(|outpoint| outpoint.index);
            assert_eq!(spent, outpoints);
            for utxo in spent_outputs.values() {
                assert_eq!(utxo.output, output());
                assert_eq!(utxo.height, height);
                assert!(!utxo.from_coinbase);
            }
        }
    };
    check_spent_outputs();

    // Prune the finalized parent's body.
    let mut batch = DiskWriteBatch::new();
    batch.zs_delete_range(
        &finalized.db.cf_handle("tx_by_loc").unwrap(),
        TransactionLocation::min_for_height(parent_height),
        TransactionLocation::min_for_height(parent_height.next().unwrap()),
    );
    finalized.db.write_batch(batch).unwrap();
    assert_eq!(
        read::block_query(
            chain.clone(),
            &finalized.db,
            parent_height.into(),
            BlockQueryFields::TRANSACTIONS
        ),
        Err(BlockReadError::BlockBodyUnavailable {
            height: parent_height
        })
    );

    check_spent_outputs();
}

/// A gap in a block's transaction index is corrupt, instead of a shortened transaction list.
#[test]
fn block_query_rejects_gaps_in_the_transaction_index() {
    let _init_guard = zebra_test::init();
    let (state, spender) = spending_chain();
    let height = spender.coinbase_height().unwrap();
    let transaction_ids: Vec<_> = spender
        .transactions
        .iter()
        .map(|transaction| transaction.hash())
        .collect();
    assert_eq!(transaction_ids.len(), 3);

    let ids = |fields| {
        read::block_query(None, &state.db, height.into(), fields)
            .map(|queried| queried.and_then(|queried| queried.transaction_ids))
    };
    assert_eq!(
        ids(BlockQueryFields::TRANSACTION_IDS),
        Ok(Some(transaction_ids.clone().into()))
    );

    let mut batch = DiskWriteBatch::new();
    batch.zs_delete(
        &state.db.cf_handle("hash_by_tx_loc").unwrap(),
        TransactionLocation::from_usize(height, 1),
    );
    state.db.write_batch(batch).unwrap();

    assert_eq!(
        ids(BlockQueryFields::TRANSACTION_IDS),
        Err(BlockReadError::CorruptBlock { height })
    );
    // The count only reads the last row, and the body read checks only the last index.
    let queried = read::block_query(
        None,
        &state.db,
        height.into(),
        BlockQueryFields::TRANSACTION_COUNT | BlockQueryFields::RAW_BLOCK,
    )
    .unwrap()
    .unwrap();
    assert_eq!(queried.transaction_count, Some(3));
    assert_eq!(
        queried.raw_block,
        Some(SerializedBlock::from(spender.as_ref()))
    );
}

/// Unknown hashes and heights above the tip are unknown without reading the history
/// capabilities. A height at or below the tip without a hash row reads them, and stays unknown
/// until snapshot-born nodes record their lowest known height.
#[test]
fn block_query_reads_history_only_for_missing_heights_at_or_below_the_tip() {
    let _init_guard = zebra_test::init();
    let blocks = mainnet_blocks();
    let state = finalized_state(&blocks);
    let tip = blocks.last().unwrap().coinbase_height().unwrap();
    let missing = Height(5);
    let reads = || HISTORY_CAPABILITIES_READS.with(|reads| reads.get());
    let block_query = |hash_or_height| {
        read::block_query(None, &state.db, hash_or_height, BlockQueryFields::all())
    };

    let before = reads();
    assert_eq!(block_query(tip.next().unwrap().into()), Ok(None));
    assert_eq!(block_query(block::Hash([0xff; 32]).into()), Ok(None));
    assert!(block_query(missing.into()).unwrap().is_some());
    assert_eq!(reads(), before);

    let mut batch = DiskWriteBatch::new();
    batch.zs_delete(&state.db.cf_handle("hash_by_height").unwrap(), missing);
    state.db.write_batch(batch).unwrap();

    assert_eq!(block_query(missing.into()), Ok(None));
    assert_eq!(reads(), before + 1);

    // An invalid retained suffix cannot show that the height is below the known history.
    let transactions = state.db.cf_handle("tx_by_loc").unwrap();
    let mut batch = DiskWriteBatch::new();
    batch.zs_delete(
        &transactions,
        TransactionLocation::min_for_height(Height(1)),
    );
    batch.zs_insert(
        &transactions,
        TransactionLocation::from_index(Height(1), 1),
        (),
    );
    state.db.write_batch(batch).unwrap();
    assert!(matches!(
        state.db.history_capabilities(),
        Err(StateInitError::HistoryRead { .. })
    ));

    assert_eq!(block_query(missing.into()), Ok(None));
    assert_eq!(reads(), before + 2);
}

/// A database failure reading the history capabilities panics, like the body reads, instead of
/// reporting a height at or below the tip as unknown.
#[test]
#[should_panic(expected = "unexpected database failure")]
fn block_query_panics_on_history_database_failures() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let path = cache.path().join("missing");
    let source = rocksdb::DB::open(&rocksdb::Options::default(), &path)
        .expect_err("opening a missing database without creating it fails");

    read::block_query::below_known_history_error(
        Err(StateInitError::HistoryRead {
            path,
            source: source.into(),
        }),
        Height(5),
    );
}

/// A chain block's parent `BlockInfo` is read only from the chain, so a stale chain never pairs
/// its blocks with a finalized fork's row at the same height. Only the chain root's parent, the
/// finalized tip, is read from the finalized state.
#[test]
fn block_query_reads_previous_block_info_of_chain_blocks_from_the_chain() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let finalized_blocks = mainnet_blocks();
    let finalized = finalized_state(&finalized_blocks);
    let finalized_tip = finalized_blocks.last().unwrap().coinbase_height().unwrap();

    let root = finalized_blocks.last().unwrap().make_fake_child();
    let child = root.make_fake_child();
    let mut cached = NonFinalizedState::new(&network);
    cached
        .commit_new_chain(root.clone().prepare(), &finalized)
        .unwrap();
    cached
        .commit_block(child.clone().prepare(), &finalized)
        .unwrap();

    let root_height = root.coinbase_height().unwrap();
    let mut chain = Chain::clone(cached.best_chain().unwrap());
    assert!(chain.block_info_by_height.remove(&root_height).is_some());
    let chain = Some(Arc::new(chain));

    // A finalized fork's row at the chain root's height.
    let decoy = BlockInfo::new(ValueBalance::fake_populated_pool(), 1);
    let mut batch = DiskWriteBatch::new();
    batch.zs_insert(
        &finalized.db.cf_handle("block_info").unwrap(),
        root_height,
        &decoy,
    );
    finalized.db.write_batch(batch).unwrap();
    assert_eq!(finalized.db.block_info(root_height.into()), Some(decoy));

    let previous_block_info = |block: &Arc<Block>| {
        read::block_query(
            chain.clone(),
            &finalized.db,
            block.hash().into(),
            BlockQueryFields::PREVIOUS_BLOCK_INFO,
        )
        .unwrap()
        .unwrap()
        .previous_block_info
    };
    assert_eq!(previous_block_info(&child), None);

    let finalized_tip_info = finalized.db.block_info(finalized_tip.into());
    assert!(finalized_tip_info.is_some());
    assert_eq!(previous_block_info(&root), finalized_tip_info);
}
