//! StateService test vectors.

#![allow(clippy::unwrap_in_result)]

// TODO: move these tests into tests::vectors and tests::prop modules.

use std::{
    env,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::runtime::Runtime;
use tower::{buffer::Buffer, util::BoxService, Service, ServiceExt as _};

use zebra_chain::{
    amount::DeferredPoolBalanceChange,
    block::{self, Block, CountedHeader, Height},
    chain_tip::ChainTip,
    fmt::SummaryDebug,
    parameters::{
        testnet::{ConfiguredActivationHeights, ParametersBuilder},
        Network, NetworkUpgrade,
    },
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    transaction, transparent,
    value_balance::ValueBalance,
    LedgerState,
};

use zebra_test::{prelude::*, transcript::Transcript};

use crate::{
    arbitrary::Prepare,
    init_test,
    service::{
        arbitrary::{populated_state, populated_state_with_debug, PreparedChain},
        chain_tip::TipAction,
        StateService,
    },
    tests::{
        setup::{partial_nu5_chain_strategy, transaction_v4_from_coinbase},
        FakeChainHelper,
    },
    BoxError, CheckpointVerifiedBlock, CommitBlockError, Config, MinedBlockNotifications,
    ReadRequest, ReadResponse, Request, Response, SemanticallyVerifiedBlock, ValidateContextError,
};

mod header;
mod mining_candidate;
pub(super) mod mining_rejection;
mod tree_roots_admission;

const LAST_BLOCK_HEIGHT: u32 = 10;

#[tokio::test(flavor = "multi_thread")]
async fn state_propagates_incomplete_block_body_errors() {
    use crate::{BlockReadError, DiskWriteBatch, TransactionLocation, WriteDisk};

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
            .values()
            .take(2)
            .map(|bytes| bytes.zcash_deserialize_into().unwrap())
            .collect();
        let block = blocks.last().unwrap().clone();
        let height = block.coinbase_height().unwrap();
        let (state, read_state, _, _) =
            populated_state_with_debug(blocks, &Network::Mainnet, true).await;
        let db = read_state.db();
        let mut batch = DiskWriteBatch::new();
        batch.zs_delete_range(
            &db.cf_handle("tx_by_loc").unwrap(),
            TransactionLocation::min_for_height(height),
            TransactionLocation::min_for_height(height.next().unwrap()),
        );
        // Exercise the size fallback used before block-info backfilling has finished.
        batch.zs_delete(&db.cf_handle("block_info").unwrap(), height);
        db.write_batch(batch).unwrap();

        let expected = BlockReadError::CorruptBlock { height };
        for query in [height.into(), block.hash().into()] {
            for request in [
                ReadRequest::Block(query),
                ReadRequest::AnyChainBlock(query),
                ReadRequest::BlockAndSize(query),
                ReadRequest::SpentOutputs(query),
                ReadRequest::TipBlockSize,
            ] {
                let error = read_state.clone().oneshot(request).await.unwrap_err();
                assert_eq!(error.downcast_ref::<BlockReadError>(), Some(&expected));
            }
            let error = state
                .clone()
                .oneshot(Request::Block(query))
                .await
                .unwrap_err();
            assert_eq!(error.downcast_ref::<BlockReadError>(), Some(&expected));
        }
        let tx_hash = block.transactions[0].hash();
        for request in [
            ReadRequest::Transaction(tx_hash),
            ReadRequest::AnyChainTransaction(tx_hash),
        ] {
            let error = read_state.clone().oneshot(request).await.unwrap_err();
            assert_eq!(error.downcast_ref::<BlockReadError>(), Some(&expected));
        }

        let ancestors = super::block_iter::any_ancestor_blocks(
            &read_state.latest_non_finalized_state(),
            db,
            block.hash(),
        );
        let error =
            super::check::legacy_chain(Height(0), ancestors, &Network::Mainnet, 10).unwrap_err();
        assert!(error.to_string().contains("cannot prove"));
        assert!(error
            .to_string()
            .contains("restore an archive snapshot or resync"));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn header_reads_distinguish_unknown_blocks_from_missing_known_headers() {
    use crate::{BlockReadError, DiskWriteBatch, WriteDisk};

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
            .values()
            .take(2)
            .map(|bytes| bytes.zcash_deserialize_into().unwrap())
            .collect();
        let block = blocks[1].clone();
        let height = block.coinbase_height().unwrap();
        let (state, read_state, _, _) =
            populated_state_with_debug(blocks, &Network::Mainnet, true).await;
        let header = crate::BlockHeader {
            header: block.header.clone(),
            hash: block.hash(),
            height,
            next_block_hash: None,
        };
        for query in [height.into(), block.hash().into()] {
            assert_eq!(
                read_state
                    .clone()
                    .oneshot(ReadRequest::BlockHeader(query))
                    .await
                    .unwrap(),
                ReadResponse::BlockHeader(Some(header.clone()))
            );
            assert_eq!(
                state
                    .clone()
                    .oneshot(Request::BlockHeader(query))
                    .await
                    .unwrap(),
                Response::BlockHeader(Some(header.clone()))
            );
        }
        for query in [Height(2).into(), block::Hash([0xff; 32]).into()] {
            assert_eq!(
                read_state
                    .clone()
                    .oneshot(ReadRequest::BlockHeader(query))
                    .await
                    .unwrap(),
                ReadResponse::BlockHeader(None)
            );
            assert_eq!(
                state
                    .clone()
                    .oneshot(Request::BlockHeader(query))
                    .await
                    .unwrap(),
                Response::BlockHeader(None)
            );
        }

        let db = read_state.db();
        let mut batch = DiskWriteBatch::new();
        batch.zs_delete(&db.cf_handle("block_header_by_height").unwrap(), height);
        db.write_batch(batch).unwrap();
        let expected = BlockReadError::CorruptBlock { height };
        for query in [height.into(), block.hash().into()] {
            let error = read_state
                .clone()
                .oneshot(ReadRequest::BlockHeader(query))
                .await
                .unwrap_err();
            assert_eq!(error.downcast_ref::<BlockReadError>(), Some(&expected));
            let error = state
                .clone()
                .oneshot(Request::BlockHeader(query))
                .await
                .unwrap_err();
            assert_eq!(error.downcast_ref::<BlockReadError>(), Some(&expected));
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn state_distinguishes_known_pruned_transactions_from_unknown_hashes() {
    use crate::{BlockReadError, DiskWriteBatch, TransactionLocation, WriteDisk};

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
            .values()
            .take(3)
            .map(|bytes| bytes.zcash_deserialize_into().unwrap())
            .collect();
        let pruned_hash = blocks[1].transactions[0].hash();
        let retained_hash = blocks[2].transactions[0].hash();
        let (state, read_state, _, _) =
            populated_state_with_debug(blocks, &Network::Mainnet, true).await;
        let db = read_state.db();
        let mut batch = DiskWriteBatch::new();
        batch.zs_delete_range(
            &db.cf_handle("tx_by_loc").unwrap(),
            TransactionLocation::min_for_height(Height(1)),
            TransactionLocation::min_for_height(Height(2)),
        );
        db.write_batch(batch).unwrap();

        let expected = BlockReadError::BlockBodyUnavailable { height: Height(1) };
        for request in [
            ReadRequest::Transaction(pruned_hash),
            ReadRequest::AnyChainTransaction(pruned_hash),
        ] {
            let error = read_state.clone().oneshot(request).await.unwrap_err();
            assert_eq!(error.downcast_ref::<BlockReadError>(), Some(&expected));
        }
        for request in [
            Request::Transaction(pruned_hash),
            Request::AnyChainTransaction(pruned_hash),
        ] {
            let error = state.clone().oneshot(request).await.unwrap_err();
            assert_eq!(error.downcast_ref::<BlockReadError>(), Some(&expected));
        }
        let unknown = transaction::Hash([0xff; 32]);
        assert!(matches!(
            read_state
                .clone()
                .oneshot(ReadRequest::Transaction(unknown))
                .await
                .unwrap(),
            ReadResponse::Transaction(None)
        ));
        assert!(matches!(
            read_state
                .clone()
                .oneshot(ReadRequest::AnyChainTransaction(unknown))
                .await
                .unwrap(),
            ReadResponse::AnyChainTransaction(None)
        ));
        assert!(matches!(
            read_state
                .clone()
                .oneshot(ReadRequest::Transaction(retained_hash))
                .await
                .unwrap(),
            ReadResponse::Transaction(Some(_))
        ));
        assert!(matches!(
            read_state
                .clone()
                .oneshot(ReadRequest::AnyChainTransaction(retained_hash))
                .await
                .unwrap(),
            ReadResponse::AnyChainTransaction(Some(_))
        ));
        assert!(matches!(
            read_state
                .oneshot(ReadRequest::KnownTransaction(pruned_hash))
                .await
                .unwrap(),
            ReadResponse::KnownTransaction(true)
        ));
    })
    .await
    .unwrap();
}

#[test]
fn legacy_chain_rejects_exhausted_history() {
    let block = zebra_test::vectors::MAINNET_BLOCKS[&1]
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    let error = super::check::legacy_chain(
        Height(0),
        std::iter::once(Ok(block)),
        &Network::Mainnet,
        100_000,
    )
    .unwrap_err();
    assert!(error.to_string().contains("cannot prove"));
}

async fn test_populated_state_responds_correctly(
    mut state: Buffer<BoxService<Request, Response, BoxError>, Request>,
) -> Result<()> {
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::MAINNET_BLOCKS
        .range(0..=LAST_BLOCK_HEIGHT)
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let block_hashes: Vec<block::Hash> = blocks.iter().map(|block| block.hash()).collect();
    let block_headers: Vec<CountedHeader> = blocks
        .iter()
        .map(|block| CountedHeader {
            header: block.header.clone(),
        })
        .collect();

    for (ind, block) in blocks.into_iter().enumerate() {
        let mut transcript = vec![];
        let height = block.coinbase_height().unwrap();
        let hash = block.hash();

        transcript.push((
            Request::Depth(block.hash()),
            Ok(Response::Depth(Some(LAST_BLOCK_HEIGHT - height.0))),
        ));

        // these requests don't have any arguments, so we just do them once
        if ind == LAST_BLOCK_HEIGHT as usize {
            transcript.push((Request::Tip, Ok(Response::Tip(Some((height, hash))))));

            let locator_hashes = vec![
                block_hashes[LAST_BLOCK_HEIGHT as usize],
                block_hashes[(LAST_BLOCK_HEIGHT - 1) as usize],
                block_hashes[(LAST_BLOCK_HEIGHT - 2) as usize],
                block_hashes[(LAST_BLOCK_HEIGHT - 4) as usize],
                block_hashes[(LAST_BLOCK_HEIGHT - 8) as usize],
                block_hashes[0],
            ];

            transcript.push((
                Request::BlockLocator,
                Ok(Response::BlockLocator(locator_hashes)),
            ));
        }

        // Spec: transactions in the genesis block are ignored.
        if height.0 != 0 {
            for transaction in &block.transactions {
                let transaction_hash = transaction.hash();

                transcript.push((
                    Request::Transaction(transaction_hash),
                    Ok(Response::Transaction(Some(transaction.clone()))),
                ));
            }
        }

        transcript.push((
            Request::Block(hash.into()),
            Ok(Response::Block(Some(block.clone()))),
        ));

        transcript.push((
            Request::Block(height.into()),
            Ok(Response::Block(Some(block.clone()))),
        ));

        // Spec: transactions in the genesis block are ignored.
        if height.0 != 0 {
            for transaction in &block.transactions {
                let transaction_hash = transaction.hash();

                let from_coinbase = transaction.is_coinbase();
                for (index, output) in transaction.outputs().iter().cloned().enumerate() {
                    let outpoint = transparent::OutPoint::from_usize(transaction_hash, index);

                    let utxo = transparent::Utxo {
                        output,
                        height,
                        from_coinbase,
                    };

                    transcript.push((
                        Request::AwaitUtxos(vec![outpoint, outpoint]),
                        Ok(Response::Utxos(vec![utxo.clone(), utxo.clone()])),
                    ));
                    transcript.push((Request::AwaitUtxo(outpoint), Ok(Response::Utxo(utxo))));
                }
            }
        }

        let mut append_locator_transcript = |split_ind| {
            let block_hashes = block_hashes.clone();
            let (known_hashes, next_hashes) = block_hashes.split_at(split_ind);

            let block_headers = block_headers.clone();
            let (_, next_headers) = block_headers.split_at(split_ind);

            // no stop
            transcript.push((
                Request::FindBlockHashes {
                    known_blocks: known_hashes.iter().rev().cloned().collect(),
                    stop: None,
                },
                Ok(Response::BlockHashes(next_hashes.to_vec())),
            ));

            transcript.push((
                Request::FindBlockHeaders {
                    known_blocks: known_hashes.iter().rev().cloned().collect(),
                    stop: None,
                },
                Ok(Response::BlockHeaders(next_headers.to_vec())),
            ));

            // stop at the next block
            transcript.push((
                Request::FindBlockHashes {
                    known_blocks: known_hashes.iter().rev().cloned().collect(),
                    stop: next_hashes.first().cloned(),
                },
                Ok(Response::BlockHashes(
                    next_hashes.first().iter().cloned().cloned().collect(),
                )),
            ));

            transcript.push((
                Request::FindBlockHeaders {
                    known_blocks: known_hashes.iter().rev().cloned().collect(),
                    stop: next_hashes.first().cloned(),
                },
                Ok(Response::BlockHeaders(
                    next_headers.first().iter().cloned().cloned().collect(),
                )),
            ));

            // stop at a block that isn't actually in the chain
            // tests bug #2789
            transcript.push((
                Request::FindBlockHashes {
                    known_blocks: known_hashes.iter().rev().cloned().collect(),
                    stop: Some(block::Hash([0xff; 32])),
                },
                Ok(Response::BlockHashes(next_hashes.to_vec())),
            ));

            transcript.push((
                Request::FindBlockHeaders {
                    known_blocks: known_hashes.iter().rev().cloned().collect(),
                    stop: Some(block::Hash([0xff; 32])),
                },
                Ok(Response::BlockHeaders(next_headers.to_vec())),
            ));
        };

        // split before the current block, and locate the current block
        append_locator_transcript(ind);

        // split after the current block, and locate the next block
        append_locator_transcript(ind + 1);

        let transcript = Transcript::from(transcript);
        transcript.check(&mut state).await?;
    }

    Ok(())
}

#[tokio::main]
async fn populate_and_check(blocks: Vec<Arc<Block>>) -> Result<()> {
    let (state, _, _, _) = populated_state(blocks, &Network::Mainnet).await;
    test_populated_state_responds_correctly(state).await?;
    Ok(())
}

fn out_of_order_committing_strategy() -> BoxedStrategy<Vec<Arc<Block>>> {
    let blocks = zebra_test::vectors::MAINNET_BLOCKS
        .range(0..=LAST_BLOCK_HEIGHT)
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into::<Arc<Block>>().unwrap())
        .collect::<Vec<_>>();

    Just(blocks).prop_shuffle().boxed()
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_state_still_responds_to_requests() -> Result<()> {
    let _init_guard = zebra_test::init();

    let block =
        zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into::<Arc<Block>>()?;

    let iter = vec![
        // No checks for SemanticallyVerifiedBlock or CommitCheckpointVerifiedBlock because empty state
        // precondition doesn't matter to them
        (Request::Depth(block.hash()), Ok(Response::Depth(None))),
        (Request::Tip, Ok(Response::Tip(None))),
        (Request::BlockLocator, Ok(Response::BlockLocator(vec![]))),
        (
            Request::Transaction(transaction::Hash([0; 32])),
            Ok(Response::Transaction(None)),
        ),
        (
            Request::Block(block.hash().into()),
            Ok(Response::Block(None)),
        ),
        (
            Request::Block(block.coinbase_height().unwrap().into()),
            Ok(Response::Block(None)),
        ),
        // No check for AwaitUTXO because it will wait if the UTXO isn't present
        (
            Request::FindBlockHashes {
                known_blocks: vec![block.hash()],
                stop: None,
            },
            Ok(Response::BlockHashes(Vec::new())),
        ),
        (
            Request::FindBlockHeaders {
                known_blocks: vec![block.hash()],
                stop: None,
            },
            Ok(Response::BlockHeaders(Vec::new())),
        ),
    ]
    .into_iter();
    let transcript = Transcript::from(iter);

    let network = Network::Mainnet;
    let state = init_test(&network).await;

    transcript.check(state).await?;

    Ok(())
}

/// Cancelling admission must not free capacity while the blocking computation is still queued.
#[test]
fn cancelled_mining_candidate_keeps_queued_computation_bounded() {
    let _init_guard = zebra_test::init();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .expect("the single-threaded test runtime should initialize");

    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (state, read_state, _, _) = crate::init_test_services(&Network::Mainnet).await;
            let ReadResponse::MiningCandidateChanges(subscription) = read_state
                .clone()
                .oneshot(ReadRequest::MiningCandidateChanges)
                .await
                .expect("the state should accept a mining subscription")
            else {
                panic!("expected a persistent mining subscription");
            };
            let block = zebra_test::vectors::BLOCK_MAINNET_419200_BYTES
                .zcash_deserialize_into::<Arc<Block>>()
                .expect("the test vector is a valid block");

            let (release_blocker, blocker_released) = tokio::sync::oneshot::channel::<()>();
            let (blocker_started, started) = tokio::sync::oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = blocker_started.send(());
                // Dropping the sender also releases this task if an assertion or timeout fails.
                let _ = blocker_released.blocking_recv();
            });
            started
                .await
                .expect("the blocking-pool occupant should start");

            let Response::MiningCandidate(Some(guard)) = state
                .clone()
                .oneshot(Request::MiningCandidate(block.clone()))
                .await
                .expect("candidate admission should not wait for the blocking pool")
            else {
                panic!("the first candidate should reserve the available computation slot");
            };

            let permit = Arc::downgrade(
                guard
                    .0
                    .borrow()
                    .as_ref()
                    .expect("the admitted candidate reserves its slot"),
            );
            while permit.strong_count() < 2 {
                tokio::task::yield_now().await;
            }
            drop(guard);
            // The publication task has exited when only the queued computation owns the permit.
            while permit.strong_count() != 1 {
                tokio::task::yield_now().await;
            }
            assert_eq!(
                Arc::strong_count(&block),
                2,
                "the cancelled candidate's blocking closure must still be queued"
            );

            assert!(
                matches!(
                    state
                        .oneshot(Request::MiningCandidate(block.clone()))
                        .await
                        .expect("a busy computation slot should reject admission immediately"),
                    Response::MiningCandidate(None)
                ),
                "cancelling the outer task must not admit another blocking computation"
            );

            release_blocker
                .send(())
                .expect("the blocking-pool occupant is still waiting");
            blocker
                .await
                .expect("the blocking-pool occupant should exit");
            // This read queues behind the cancelled computation on the single blocking thread.
            assert!(matches!(
                read_state
                    .oneshot(ReadRequest::MiningCandidate)
                    .await
                    .expect("the blocking queue should drain after release"),
                ReadResponse::MiningCandidate(None)
            ));
            assert_eq!(
                Arc::strong_count(&block),
                1,
                "the cancelled computation must release its block after the queue drains"
            );
            drop(subscription);
        })
        .await
        .expect("candidate admission and cancellation should finish within the timeout");
    });
}

/// A live or expired candidate reserves its slot until its verifier drops the guard.
#[tokio::test]
async fn mining_candidate_slot_is_held_until_verification_finishes() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(Duration::from_secs(60), async {
        let network = Network::Mainnet;
        let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .unwrap();
        let (state, read_state, _, _) = populated_state([genesis], &network).await;
        let ReadResponse::ChainInfo(context) = read_state
            .clone()
            .oneshot(ReadRequest::ChainInfo)
            .await
            .unwrap()
        else {
            panic!("state must return chain info");
        };
        let mut block = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .unwrap();
        let header = Arc::make_mut(&mut block.header);
        header.time = context.min_time.into();
        header.difficulty_threshold = context.expected_difficulty;
        let block = Arc::new(block);
        let ReadResponse::MiningCandidateChanges(mut changes) = read_state
            .clone()
            .oneshot(ReadRequest::MiningCandidateChanges)
            .await
            .unwrap()
        else {
            panic!("state must return a candidate subscription");
        };
        let Response::MiningCandidate(Some(mut guard)) = state
            .clone()
            .oneshot(Request::MiningCandidate(block.clone()))
            .await
            .unwrap()
        else {
            panic!("the first candidate must reserve the available slot");
        };
        changes.changed().await;
        assert!(Arc::ptr_eq(&changes.block(block.hash()).unwrap(), &block));
        assert!(changes.block(block::Hash([0xff; 32])).is_none());

        let mut replacement = (*block).clone();
        Arc::make_mut(&mut replacement.header).nonce = [1; 32].into();
        assert!(matches!(
            state
                .clone()
                .oneshot(Request::MiningCandidate(Arc::new(replacement)))
                .await
                .unwrap(),
            Response::MiningCandidate(None)
        ));
        assert!(matches!(
            read_state
                .clone()
                .oneshot(ReadRequest::MiningCandidate)
                .await
                .unwrap(),
            ReadResponse::MiningCandidate(Some(info)) if info.tip_hash == block.hash()
        ));

        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(31)).await;
        tokio::time::resume();
        changes.changed().await;
        assert!(changes.block(block.hash()).is_none());
        assert!(matches!(
            read_state
                .oneshot(ReadRequest::MiningCandidate)
                .await
                .unwrap(),
            ReadResponse::MiningCandidate(None)
        ));
        guard
            .0
            .changed()
            .await
            .expect_err("expiry closes publication without releasing the verifier's slot");
        assert!(matches!(
            state
                .clone()
                .oneshot(Request::MiningCandidate(block.clone()))
                .await
                .unwrap(),
            Response::MiningCandidate(None)
        ));

        drop(guard);
        assert!(matches!(
            state
                .oneshot(Request::MiningCandidate(block))
                .await
                .unwrap(),
            Response::MiningCandidate(Some(_))
        ));
    })
    .await
    .expect("candidate expiry must not release the slot before verification finishes");
}

/// A live verifier cannot authorize a candidate after its sibling becomes the committed tip.
#[tokio::test]
async fn mining_candidate_is_withdrawn_when_a_sibling_commits() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(20), async {
        let network = Network::Mainnet;
        let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .unwrap();
        let (state, read_state, _, _) = populated_state([genesis], &network).await;
        let ReadResponse::ChainInfo(context) = read_state
            .clone()
            .oneshot(ReadRequest::ChainInfo)
            .await
            .unwrap()
        else {
            panic!("state returns the requested chain info");
        };
        let sibling = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .unwrap();
        let mut candidate = (*sibling).clone();
        let header = Arc::make_mut(&mut candidate.header);
        header.time = context.min_time.into();
        header.difficulty_threshold = context.expected_difficulty;
        header.nonce = [1; 32].into();
        let candidate = Arc::new(candidate);
        let hash = candidate.hash();
        let ReadResponse::MiningCandidateChanges(mut changes) = read_state
            .clone()
            .oneshot(ReadRequest::MiningCandidateChanges)
            .await
            .unwrap()
        else {
            panic!("state returns the requested candidate subscription");
        };
        let Response::MiningCandidate(Some(guard)) = state
            .clone()
            .oneshot(Request::MiningCandidate(candidate))
            .await
            .unwrap()
        else {
            panic!("the subscription enables candidate publication");
        };
        changes.changed().await;
        assert!(changes.is_current(hash));
        // Hold the publication independently of the withdrawal task to exercise final authorization.
        let (_publication, receiver) = tokio::sync::watch::channel(changes.0.borrow().clone());
        let mut detached = changes.clone();
        detached.0 = receiver;
        state
            .clone()
            .oneshot(Request::CommitCheckpointVerifiedBlock(sibling.into()))
            .await
            .unwrap();
        assert!(
            !detached.is_current(hash),
            "authorization must consult committed state even before withdrawal is observed"
        );
        changes.changed().await;
        assert!(
            changes.0.borrow().is_none(),
            "the obsolete publication must be withdrawn"
        );
        assert!(matches!(
            read_state
                .oneshot(ReadRequest::MiningCandidate)
                .await
                .unwrap(),
            ReadResponse::MiningCandidate(None)
        ));
        drop(guard);
    })
    .await
    .expect("sibling commitment must withdraw the candidate before its verifier finishes");
}

#[test]
fn state_behaves_when_blocks_are_committed_in_order() -> Result<()> {
    let _init_guard = zebra_test::init();

    let blocks = zebra_test::vectors::MAINNET_BLOCKS
        .range(0..=LAST_BLOCK_HEIGHT)
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into::<Arc<Block>>().unwrap())
        .collect();

    populate_and_check(blocks)?;

    Ok(())
}

const DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES: u32 = 2;

/// The legacy chain limit for tests.
const TEST_LEGACY_CHAIN_LIMIT: usize = 100;

/// Check more blocks than the legacy chain limit.
const OVER_LEGACY_CHAIN_LIMIT: u32 = TEST_LEGACY_CHAIN_LIMIT as u32 + 10;

/// Check fewer blocks than the legacy chain limit.
const UNDER_LEGACY_CHAIN_LIMIT: u32 = TEST_LEGACY_CHAIN_LIMIT as u32 - 10;

proptest! {
    #![proptest_config(
        proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES))
    )]

    /// Test out of order commits of continuous block test vectors from genesis onward.
    #[test]
    fn state_behaves_when_blocks_are_committed_out_of_order(blocks in out_of_order_committing_strategy()) {
        let _init_guard = zebra_test::init();

        populate_and_check(blocks).unwrap();
    }

    /// Test blocks that are less than the NU5 activation height.
    #[test]
    fn some_block_less_than_network_upgrade(
        (network, _, chain) in partial_nu5_chain_strategy(4, true, UNDER_LEGACY_CHAIN_LIMIT, NetworkUpgrade::Canopy)
    ) {
        let nu_activation_height = NetworkUpgrade::Nu5
            .activation_height(&network)
            .expect("test networks activate NU5 after Canopy");
        let response = crate::service::check::legacy_chain(nu_activation_height, chain.into_iter().rev().map(Ok), &network, TEST_LEGACY_CHAIN_LIMIT)
            .map_err(|error| error.to_string());

        prop_assert_eq!(response, Ok(None));
    }

    /// Test the maximum amount of blocks to check before chain is declared to be legacy.
    #[test]
    fn no_transaction_with_network_upgrade(
        (network, nu_activation_height, chain) in partial_nu5_chain_strategy(4, true, OVER_LEGACY_CHAIN_LIMIT, NetworkUpgrade::Canopy)
    ) {
        let tip_height = chain
            .last()
            .expect("chain contains at least one block")
            .coinbase_height()
            .expect("chain contains valid blocks");

        let response = crate::service::check::legacy_chain(nu_activation_height, chain.into_iter().rev().map(Ok), &network, TEST_LEGACY_CHAIN_LIMIT)
            .map(|_| ())
            .map_err(|error| error.to_string());

        prop_assert_eq!(
            response,
            Err(format!(
                "could not find any transactions in recent blocks: checked {TEST_LEGACY_CHAIN_LIMIT} blocks back from {tip_height:?}",
            ))
        );
    }

    /// Test the `Block.check_transaction_network_upgrade()` error inside the legacy check.
    #[test]
    fn at_least_one_transaction_with_inconsistent_network_upgrade(
        (network, nu_activation_height, chain) in partial_nu5_chain_strategy(5, false, OVER_LEGACY_CHAIN_LIMIT, NetworkUpgrade::Nu5)
    ) {
        // this test requires that an invalid block is encountered
        // before a valid block (and before the check gives up),
        // but setting `transaction_has_valid_network_upgrade` to false
        // sometimes generates blocks with all valid (or missing) network upgrades

        // we must check at least one block, and the first checked block must be invalid
        let first_checked_block = chain
            .iter()
            .rev()
            .take_while(|block| block.coinbase_height().unwrap() >= nu_activation_height)
            .take(100)
            .next();
        prop_assume!(first_checked_block.is_some());
        prop_assume!(
            first_checked_block
                .unwrap()
                .check_transaction_network_upgrade_consistency(&network)
                .is_err()
        );

        let response = crate::service::check::legacy_chain(
            nu_activation_height,
            chain.clone().into_iter().rev().map(Ok),
            &network,
            TEST_LEGACY_CHAIN_LIMIT,
        ).map(|_| ()).map_err(|error| error.to_string());

        prop_assert_eq!(
            response,
            Err("inconsistent network upgrade found in transaction: WrongTransactionConsensusBranchId".into()),
            "first: {:?}, last: {:?}",
            chain.first().map(|block| block.coinbase_height()),
            chain.last().map(|block| block.coinbase_height()),
        );
    }

    /// Test there is at least one transaction with a valid `network_upgrade` in the legacy check.
    #[test]
    fn at_least_one_transaction_with_valid_network_upgrade(
        (network, nu_activation_height, chain) in partial_nu5_chain_strategy(5, true, UNDER_LEGACY_CHAIN_LIMIT, NetworkUpgrade::Nu5)
    ) {
        let response = crate::service::check::legacy_chain(nu_activation_height, chain.into_iter().rev().map(Ok), &network, TEST_LEGACY_CHAIN_LIMIT)
            .map(|_| ())
            .map_err(|error| error.to_string());

        prop_assert_eq!(response, Ok(()));
    }

    /// Test that the value pool is updated accordingly.
    ///
    /// 1. Generate a finalized chain and some non-finalized blocks.
    /// 2. Check that initially the value pool is empty.
    /// 3. Commit the finalized blocks and check that the value pool is updated accordingly.
    /// 4. Commit the non-finalized blocks and check that the value pool is also updated
    ///    accordingly.
    #[test]
    fn value_pool_is_updated(
        (network, finalized_blocks, non_finalized_blocks)
            in continuous_empty_blocks_from_test_vectors(),
    ) {
        let _init_guard = zebra_test::init();
        let (mut state_service, _, _, _) = Runtime::new().unwrap().block_on(async {
            // We're waiting to verify each block here, so we don't need the maximum checkpoint height.
            StateService::new(Config::ephemeral(), &network, Height::MAX, 0).await
        });

        prop_assert_eq!(state_service.read_service.db.finalized_value_pool(), ValueBalance::zero());
        prop_assert_eq!(
            state_service.read_service.latest_non_finalized_state().best_chain().map(|chain| chain.chain_value_pools).unwrap_or_else(ValueBalance::zero),
            ValueBalance::zero()
        );

        // the slow start rate for the first few blocks, as in the spec
        const SLOW_START_RATE: i64 = 62500;
        // the expected transparent pool value, calculated using the slow start rate
        let mut expected_transparent_pool = ValueBalance::zero();

        let mut expected_finalized_value_pool = Ok(ValueBalance::zero());
        for block in finalized_blocks {
            // the genesis block has a zero-valued transparent output,
            // which is not included in the UTXO set
            if block.height > block::Height(0) {
                let utxos = &block.new_outputs.iter().map(|(k, ordered_utxo)| (*k, ordered_utxo.utxo.clone())).collect();
                let block_value_pool = &block.block.chain_value_pool_change(utxos, DeferredPoolBalanceChange::zero(), &network, expected_finalized_value_pool.clone()?.constrain()?)?;
                expected_finalized_value_pool += *block_value_pool;
            }

            let result_receiver = state_service.queue_and_commit_to_finalized_state(block.clone(), None);
            let result = result_receiver.blocking_recv();

            prop_assert!(result.is_ok(), "unexpected failed finalized block commit: {:?}", result);

            prop_assert_eq!(
                state_service.read_service.db.finalized_value_pool(),
                expected_finalized_value_pool.clone()?.constrain()?
            );

            let transparent_value = SLOW_START_RATE * i64::from(block.height.0);
            let transparent_value = transparent_value.try_into().unwrap();
            let transparent_value = ValueBalance::from_transparent_amount(transparent_value);
            expected_transparent_pool = (expected_transparent_pool + transparent_value).unwrap();
            prop_assert_eq!(
                state_service.read_service.db.finalized_value_pool(),
                expected_transparent_pool
            );
        }

        let mut expected_non_finalized_value_pool = Ok(expected_finalized_value_pool?);
        for block in non_finalized_blocks {
            // Portions copyright (c) 2026 Zakura Contributors, licensed under the MIT license or
            // the Apache License, Version 2.0, at your option.
            // Adapted from zakura-core/zakura#1905
            // (https://github.com/zakura-core/zakura/pull/1905).
            let utxos = block.new_outputs.as_ref().clone();
            let block_value_pool = &block.block.chain_value_pool_change(&transparent::utxos_from_ordered_utxos(utxos), DeferredPoolBalanceChange::zero(), &network, expected_non_finalized_value_pool.clone()?.constrain()?)?;
            expected_non_finalized_value_pool += *block_value_pool;

            let result_receiver = state_service.queue_and_commit_to_non_finalized_state(block.clone(), None, None);
            let result = result_receiver.blocking_recv();

            prop_assert!(result.is_ok(), "unexpected failed non-finalized block commit: {:?}", result);

            prop_assert_eq!(
                state_service.read_service.latest_non_finalized_state().best_chain().unwrap().chain_value_pools,
                expected_non_finalized_value_pool.clone()?.constrain()?
            );

            let transparent_value = SLOW_START_RATE * i64::from(block.height.0);
            let transparent_value = transparent_value.try_into().unwrap();
            let transparent_value = ValueBalance::from_transparent_amount(transparent_value);
            expected_transparent_pool = (expected_transparent_pool + transparent_value).unwrap();
            prop_assert_eq!(
                state_service.read_service.latest_non_finalized_state().best_chain().unwrap().chain_value_pools,
                expected_transparent_pool
            );
        }
    }
}

proptest! {
    #![proptest_config(
        proptest::test_runner::Config::with_cases(1)
    )]

    /// Test that the best tip height is updated accordingly.
    ///
    /// 1. Generate a finalized chain and some non-finalized blocks.
    /// 2. Check that initially the best tip height is empty.
    /// 3. Commit the finalized blocks, await each tip notification, and check the best tip height.
    /// 4. Commit the non-finalized blocks and check that the best tip height is also updated
    ///    accordingly.
    #[test]
    fn chain_tip_sender_is_updated(
        (network, finalized_blocks, non_finalized_blocks)
            in continuous_empty_blocks_from_test_vectors(),
    ) {
        let _init_guard = zebra_test::init();

        let runtime = Runtime::new().unwrap();
        let (mut state_service, _read_only_state_service, mut latest_chain_tip, mut chain_tip_change) =
            runtime.block_on(async {
                // Each block is committed separately, so the maximum checkpoint height is unused.
                StateService::new(Config::ephemeral(), &network, Height::MAX, 0).await
            });

        prop_assert_eq!(latest_chain_tip.best_tip_height(), None);
        prop_assert_eq!(chain_tip_change.last_tip_change(), None);

        for block in finalized_blocks {
            let expected_block = block.clone();

            let expected_action = if expected_block.height == block::Height(0) {
                // 0: reset on initialization.
                //
                // The mempool verifies transactions against the next block height, so the
                // reset for an upgrade activating at height `H` happens when the tip reaches
                // `H - 1`. The only activation height whose predecessor is in the test vectors
                // is BeforeOverwinter (height 1), so its reset coincides with the genesis block
                // at height 0, which already resets on initialization.
                TipAction::reset_with(expected_block.clone().into())
            } else {
                TipAction::grow_with(expected_block.clone().into())
            };

            let result_receiver = state_service.queue_and_commit_to_finalized_state(block, None);
            let result = result_receiver.blocking_recv();

            prop_assert!(result.is_ok(), "unexpected failed finalized block commit: {:?}", result);

            // Await publication without consuming the tip action checked below.
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(30), latest_chain_tip.best_tip_changed())
                    .await
                    .expect("the committed block's tip notification should arrive")
                    .expect("the state service keeps the tip channel open");
            });

            prop_assert_eq!(latest_chain_tip.best_tip_height(), Some(expected_block.height));
            prop_assert_eq!(chain_tip_change.last_tip_change(), Some(expected_action));
        }

        for block in non_finalized_blocks {
            let expected_block = block.clone();

            // Non-finalized blocks are continuous with the finalized tip and always at height
            // >= 1 (genesis is always finalized). The reset for an upgrade activating at height
            // `H` happens when the tip reaches `H - 1`, and the only activation height with a
            // predecessor in the test vectors is BeforeOverwinter (height 1, predecessor height
            // 0, which is always finalized). So no non-finalized block triggers an activation
            // reset: they all grow.
            let expected_action = TipAction::grow_with(expected_block.clone().into());

            let result_receiver = state_service.queue_and_commit_to_non_finalized_state(block, None, None);
            let result = result_receiver.blocking_recv();

            prop_assert!(result.is_ok(), "unexpected failed non-finalized block commit: {:?}", result);

            // Await publication without consuming the tip action checked below.
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(30), latest_chain_tip.best_tip_changed())
                    .await
                    .expect("the committed block's tip notification should arrive")
                    .expect("the state service keeps the tip channel open");
            });

            prop_assert_eq!(latest_chain_tip.best_tip_height(), Some(expected_block.height));
            prop_assert_eq!(chain_tip_change.last_tip_change(), Some(expected_action));
        }
    }
}

/// Test strategy to generate a chain split in two from the test vectors.
///
/// Selects either the mainnet or testnet chain test vector and randomly splits the chain in two
/// lists of blocks. The first containing the blocks to be finalized (which always includes at
/// least the genesis block) and the blocks to be stored in the non-finalized state.
fn continuous_empty_blocks_from_test_vectors() -> impl Strategy<
    Value = (
        Network,
        SummaryDebug<Vec<CheckpointVerifiedBlock>>,
        SummaryDebug<Vec<SemanticallyVerifiedBlock>>,
    ),
> {
    any::<Network>()
        .prop_flat_map(|network| {
            // Select the test vector based on the network
            let raw_blocks = network.blockchain_map();

            // Transform the test vector's block bytes into a vector of `SemanticallyVerifiedBlock`s.
            let blocks: Vec<_> = raw_blocks
                .iter()
                .map(|(_height, &block_bytes)| {
                    let mut block_reader: &[u8] = block_bytes;
                    let mut block = Block::zcash_deserialize(&mut block_reader)
                        .expect("Failed to deserialize block from test vector");

                    let coinbase = transaction_v4_from_coinbase(&block.transactions[0]);
                    block.transactions = vec![Arc::new(coinbase)];

                    Arc::new(block).prepare()
                })
                .collect();

            // Always finalize the genesis block
            let finalized_blocks_count = 1..=blocks.len();

            (Just(network), Just(blocks), finalized_blocks_count)
        })
        .prop_map(|(network, mut blocks, finalized_blocks_count)| {
            let non_finalized_blocks = blocks.split_off(finalized_blocks_count);
            let finalized_blocks: Vec<_> = blocks
                .into_iter()
                .map(CheckpointVerifiedBlock::from)
                .collect();

            (
                network,
                finalized_blocks.into(),
                non_finalized_blocks.into(),
            )
        })
}

/// Opening a read-only state against an existing but empty cache directory (no database on
/// disk) must fail with [`StateInitError::ReadOnlyDatabaseNotFound`] rather than silently
/// creating a new, empty database.
#[test]
fn read_only_open_with_no_database_returns_error() {
    let network = Network::Mainnet;

    // An existing, readable, but empty cache directory: it contains no database.
    let cache_dir =
        tempfile::tempdir().expect("creating a temporary cache directory should succeed");
    let config = Config {
        cache_dir: cache_dir.path().to_path_buf(),
        ephemeral: false,
        ..Config::default()
    };

    match super::init_read_only(config, &network) {
        Err(crate::StateInitError::ReadOnlyDatabaseNotFound { .. }) => {}
        Err(other) => panic!("expected ReadOnlyDatabaseNotFound, got: {other:?}"),
        Ok(_) => panic!("expected an error when opening a read-only state with no database"),
    }
}

/// Opening a read-only state against a missing or unreadable cache directory must fail with a
/// typed [`StateInitError::ReadOnlyCacheDirUnreadable`] rather than panicking while reading the
/// on-disk format version.
#[test]
fn read_only_open_with_unreadable_cache_dir_returns_error() {
    let network = Network::Mainnet;

    // A cache directory that does not exist. `read_dir` fails for a missing directory the same way
    // it does for an unreadable one, without depending on filesystem permissions (which `root`
    // ignores, so a chmod-based unreadable directory would not be a reliable test under CI).
    let parent = tempfile::tempdir().expect("creating a temporary directory should succeed");
    let config = Config {
        cache_dir: parent.path().join("missing"),
        ephemeral: false,
        ..Config::default()
    };

    match super::init_read_only(config, &network) {
        Err(crate::StateInitError::ReadOnlyCacheDirUnreadable { .. }) => {}
        Err(other) => panic!("expected ReadOnlyCacheDirUnreadable, got: {other:?}"),
        Ok(_) => {
            panic!("expected an error when opening a read-only state with an unreadable cache dir")
        }
    }
}

/// Opening a read-only state with an ephemeral database configured must fail with
/// [`StateInitError::ReadOnlyEphemeralConflict`]: a read-only secondary follows another
/// process's primary database and must never delete it, so it cannot also be ephemeral
/// (which would delete the primary's files on drop).
#[test]
fn read_only_open_with_ephemeral_config_returns_error() {
    let network = Network::Mainnet;

    // While `ephemeral: true` should make `cache_dir` irrelevant, we had
    // instances where a bug would try to read the `cache_dir` before checking
    // `ephemeral`, so we use a missing directory to ensure that `ephemeral` is
    // checked first.
    let parent = tempfile::tempdir().expect("creating a temporary directory should succeed");
    let config = Config {
        ephemeral: true,
        cache_dir: parent.path().join("missing"),
        ..Config::default()
    };

    match super::init_read_only(config, &network) {
        Err(crate::StateInitError::ReadOnlyEphemeralConflict) => {}
        Err(other) => panic!("expected ReadOnlyEphemeralConflict, got: {other:?}"),
        Ok(_) => {
            panic!("expected an error when opening a read-only state with an ephemeral config")
        }
    }
}

/// Opening a read-only state against a database without a column family in the code, like a
/// database last opened by an older Zebra version, must succeed: a read-only secondary can't
/// create column families, so it opens the ones on disk.
#[test]
fn read_only_open_without_a_newer_column_family() {
    use crate::{
        constants::STATE_DATABASE_KIND,
        service::finalized_state::{ZebraDb, STATE_COLUMN_FAMILIES_IN_CODE},
        state_database_format_version_in_code, write_state_database_format_version_to_disk,
    };

    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let cache_dir =
        tempfile::tempdir().expect("creating a temporary cache directory should succeed");
    let config = Config {
        cache_dir: cache_dir.path().to_path_buf(),
        ephemeral: false,
        ..Config::default()
    };
    let version = state_database_format_version_in_code();
    let (newer, older) = STATE_COLUMN_FAMILIES_IN_CODE
        .split_last()
        .expect("the state has column families");

    let _primary = ZebraDb::new(
        &config,
        STATE_DATABASE_KIND,
        &version,
        &network,
        true,
        older.iter().map(ToString::to_string),
        false,
    )
    .expect("creating a database should succeed");
    // Format upgrades are skipped, so they don't write the version that marks the database as
    // existing.
    write_state_database_format_version_to_disk(&config, &version, &network)
        .expect("writing the version file should succeed");

    let (_, secondary, _) = super::init_read_only(config, &network)
        .expect("a read-only state opens without newer column families");
    assert!(secondary.db().cf_handle(newer).is_none());
}

/// Returns a copy of `block` with one byte of its coinbase miner data flipped.
///
/// ZIP-244 excludes scriptSigs from v5 transaction IDs, so the block hash is unchanged,
/// but the authorizing data commitment is not.
fn coinbase_authorization_twin(block: &Arc<Block>) -> Arc<Block> {
    let mut coinbase = block.transactions[0]
        .zcash_serialize_to_vec()
        .expect("coinbase serializes");

    // A v5 header is 20 bytes, followed by the input count and the 36-byte null prevout.
    const SCRIPT_LEN_OFFSET: usize = 20 + 1 + 36;
    assert_eq!(
        coinbase[SCRIPT_LEN_OFFSET - 4..SCRIPT_LEN_OFFSET],
        [0xff; 4]
    );
    let script_end = SCRIPT_LEN_OFFSET + 1 + usize::from(coinbase[SCRIPT_LEN_OFFSET]);
    coinbase[script_end - 1] ^= 0x01;

    let coinbase: transaction::Transaction = coinbase
        .zcash_deserialize_into()
        .expect("mutated coinbase parses");

    let mut twin = block.clone();
    Arc::make_mut(&mut twin).transactions[0] = Arc::new(coinbase);
    twin
}

/// Returns a ledger strategy for a Testnet that activates NU5 at height 7, so the generated
/// chain has v5 transactions and the finalized state checks the authorizing data commitment.
fn nu5_ledger_strategy() -> BoxedStrategy<LedgerState> {
    // Keep this early-upgrade schedule within the monetary cap.
    let network = ParametersBuilder::default()
        .with_slow_start_interval(Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            before_overwinter: Some(1),
            overwinter: Some(2),
            sapling: Some(3),
            blossom: Some(4),
            heartwood: Some(5),
            canopy: Some(6),
            nu5: Some(7),
            ..Default::default()
        })
        .expect("valid activation heights")
        .extend_funding_streams()
        .expect("valid funding streams")
        .to_network()
        .expect("valid test network");

    LedgerState::genesis_strategy(Some(network), NetworkUpgrade::Nu5, 5, false)
}

/// Checks that after a checkpoint block fails the finalized commitment check, no block above the
/// finalized tip is reported as known or left waiting in the queue, so the syncer can download
/// them again and the honest range commits without a restart.
///
/// Covers blocks dropped by the write task, queued before the reset, arriving after the reset,
/// and queued behind a gap while the range is re-sent out of order.
#[test]
fn rejected_checkpoint_range_can_be_downloaded_again() -> Result<()> {
    let _init_guard = zebra_test::init();

    let ledger_strategy = nu5_ledger_strategy();

    proptest!(
        ProptestConfig::with_cases(1),
        |((chain, _count, network, _history_tree) in PreparedChain::default()
            .with_ledger_strategy(ledger_strategy)
            .with_valid_commitments()
            .no_shrink())| {
            let nu5_height = NetworkUpgrade::Nu5.activation_height(&network).unwrap();
            let honest_index = chain.iter().position(|block| block.height == nu5_height).unwrap();
            let range = &chain[honest_index..honest_index + 4];
            let parent_height = chain[honest_index - 1].height;

            let twin = coinbase_authorization_twin(&range[0].block);
            prop_assert_eq!(twin.hash(), range[0].hash);
            prop_assert_ne!(twin.auth_data_root(), range[0].block.auth_data_root());

            let runtime = Runtime::new().unwrap();
            // A large concurrency limit makes every block "close to the final checkpoint".
            let (mut state, _, _, _) = runtime.block_on(StateService::new(
                Config::ephemeral(),
                &network,
                range[3].height,
                1_000,
            ));

            for block in chain.iter().take(honest_index) {
                state
                    .queue_and_commit_to_finalized_state(CheckpointVerifiedBlock::from(block.block.clone()), None)
                    .blocking_recv()
                    .expect("write task responds")
                    .expect("valid prefix commits");
            }

            let checkpoint_verified = |index: usize| CheckpointVerifiedBlock::from(range[index].block.clone());
            let known_block = |state: &mut StateService, index: usize| {
                runtime
                    .block_on(state.call(Request::KnownBlock(range[index].hash)))
                    .expect("KnownBlock succeeds")
            };

            // The first child is sent right behind the twin, and the third one waits in the queue
            // for its parent when the twin fails.
            let twin_rx = state.queue_and_commit_to_finalized_state(CheckpointVerifiedBlock::from(twin), None);
            let child_rx = state.queue_and_commit_to_finalized_state(checkpoint_verified(1), None);
            let queued_rx = state.queue_and_commit_to_finalized_state(checkpoint_verified(3), None);

            let twin_error = twin_rx.blocking_recv().expect("write task responds").expect_err("twin is rejected");
            prop_assert!(
                matches!(
                    twin_error.inner(),
                    CommitBlockError::ValidateContextError(error)
                        if matches!(
                            **error,
                            ValidateContextError::InvalidBlockCommitment(
                                block::CommitmentError::InvalidChainHistoryBlockTxAuthCommitment { .. }
                            )
                        )
                ),
                "unexpected twin error: {twin_error:?}"
            );
            // The write task drops the child after it sends the reset, so waiting for the child's
            // response also guarantees the reset is queued before the checks below.
            prop_assert!(child_rx.blocking_recv().is_err(), "dropped child gets no response");
            prop_assert_eq!(state.read_service.db.finalized_tip_height(), Some(parent_height));

            // The second child reaches the state after the reset, ahead of the reset height.
            let late_rx = state.queue_and_commit_to_finalized_state(checkpoint_verified(2), None);

            // The reset fails the queued block, and the late block is failed instead of queued.
            for rx in [queued_rx, late_rx] {
                let error = rx
                    .blocking_recv()
                    .expect("state responds")
                    .expect_err("block above the reset is failed");
                prop_assert!(
                    matches!(error.inner(), CommitBlockError::WriteTaskExited),
                    "unexpected error: {error:?}"
                );
            }
            prop_assert!(state.finalized_state_queued_blocks.is_empty());
            for index in 0..range.len() {
                prop_assert_eq!(known_block(&mut state, index), Response::KnownBlock(None));
            }

            // The range is sent again out of order: the fourth block waits for the third one,
            // and it isn't reported as known until it is sent.
            for index in [0, 1] {
                state
                    .queue_and_commit_to_finalized_state(checkpoint_verified(index), None)
                    .blocking_recv()
                    .expect("write task responds")
                    .expect("honest block commits");
            }
            let queued_rx = state.queue_and_commit_to_finalized_state(checkpoint_verified(3), None);
            for index in [2, 3] {
                prop_assert_eq!(known_block(&mut state, index), Response::KnownBlock(None));
            }

            state
                .queue_and_commit_to_finalized_state(checkpoint_verified(2), None)
                .blocking_recv()
                .expect("write task responds")
                .expect("honest block commits");
            prop_assert_eq!(
                queued_rx.blocking_recv().expect("write task responds").expect("queued block commits"),
                range[3].hash
            );
            prop_assert_eq!(state.read_service.db.finalized_tip_height(), Some(range[3].height));
        }
    );

    Ok(())
}

/// A semantically verified block that names a checkpoint verified block below the final
/// checkpoint as its parent can't end checkpoint sync, whether it's queued after its parent is
/// committed or before. The switch happens once the final checkpoint is committed.
#[test]
fn forged_child_does_not_end_checkpoint_sync() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let blocks: Vec<Arc<Block>> = network
        .blockchain_map()
        .range(0..=9)
        .map(|(_height, bytes)| bytes.zcash_deserialize_into().map(Arc::new))
        .collect::<Result<_, _>>()?;
    let max_checkpoint_height = Height(8);

    let runtime = Runtime::new()?;
    let (mut state, _, _, _) = runtime.block_on(StateService::new(
        Config::ephemeral(),
        &network,
        max_checkpoint_height,
        0,
    ));

    let commit_checkpoint = |state: &mut StateService, height: usize| {
        state
            .queue_and_commit_to_finalized_state(
                CheckpointVerifiedBlock::from(blocks[height].clone()),
                None,
            )
            .blocking_recv()
            .expect("write task responds")
            .expect("checkpoint verified block commits");
    };
    // The real block above the final checkpoint, re-parented onto a lower checkpoint block.
    let forged_child = |parent: usize| {
        let mut block = Arc::unwrap_or_clone(blocks[9].clone());
        Arc::make_mut(&mut block.header).previous_block_hash = blocks[parent].hash();
        Arc::new(block).prepare()
    };
    let assert_forged_child_rejected = |error: crate::CommitSemanticallyVerifiedError| {
        assert!(
            matches!(
                error.inner(),
                CommitBlockError::ValidateContextError(error)
                    if matches!(**error, ValidateContextError::NonSequentialFinalizedChild { .. })
            ),
            "unexpected error: {error:?}"
        );
        assert_eq!(error.inner().misbehavior_score(), 100);
    };

    for height in 0..=5 {
        commit_checkpoint(&mut state, height);
    }
    assert!(!state.checkpoint_handoff_is_ready());

    // A child of the committed tip is rejected before it's queued.
    let rejected = state
        .queue_and_commit_to_non_finalized_state(forged_child(5), None, None)
        .try_recv()
        .expect("forged child is answered before it's queued")
        .expect_err("forged child is rejected");
    assert_forged_child_rejected(rejected);
    assert!(state.block_write_sender.finalized.is_some());

    // A child of a checkpoint block that hasn't arrived yet is rejected when its parent is sent.
    let mut queued_rx = state.queue_and_commit_to_non_finalized_state(forged_child(7), None, None);
    assert!(state.block_write_sender.finalized.is_some());
    commit_checkpoint(&mut state, 6);
    commit_checkpoint(&mut state, 7);
    let rejected = queued_rx
        .try_recv()
        .expect("forged child is answered when its parent is sent")
        .expect_err("forged child is rejected");
    assert_forged_child_rejected(rejected);
    assert!(state.block_write_sender.finalized.is_some());

    // The honest switch still happens at the final checkpoint.
    commit_checkpoint(&mut state, 8);
    assert!(state.checkpoint_handoff_is_ready());
    let _honest_rx =
        state.queue_and_commit_to_non_finalized_state(blocks[9].clone().prepare(), None, None);
    assert!(state.block_write_sender.finalized.is_none());

    Ok(())
}

/// A forged child queued while its checkpoint parent is in the write channel is rejected at the
/// switch to full validation.
#[test]
fn forged_child_of_unwritten_parent_is_rejected_at_handoff() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let blocks: Vec<Arc<Block>> = network
        .blockchain_map()
        .range(0..=9)
        .map(|(_height, bytes)| bytes.zcash_deserialize_into().map(Arc::new))
        .collect::<Result<_, _>>()?;

    let runtime = Runtime::new()?;
    let (mut state, _, _, _) = runtime.block_on(StateService::new(
        Config::ephemeral(),
        &network,
        Height(8),
        0,
    ));

    let commit_checkpoint = |state: &mut StateService, height: usize| {
        state
            .queue_and_commit_to_finalized_state(
                CheckpointVerifiedBlock::from(blocks[height].clone()),
                None,
            )
            .blocking_recv()
            .expect("write task responds")
            .expect("checkpoint verified block commits");
    };

    for height in 0..=5 {
        commit_checkpoint(&mut state, height);
    }

    // Hold block 6 in a substitute write channel, so it's sent but not written.
    let (held_tx, mut held_rx) = tokio::sync::mpsc::unbounded_channel();
    let write_sender = state
        .block_write_sender
        .finalized
        .replace(held_tx)
        .expect("checkpoint sync is running");
    let parent_rx = state.queue_and_commit_to_finalized_state(
        CheckpointVerifiedBlock::from(blocks[6].clone()),
        None,
    );
    let held_parent = held_rx.try_recv().expect("block 6 is sent");
    assert_eq!(state.read_service.db.height(blocks[6].hash()), None);

    let mut forged = Arc::unwrap_or_clone(blocks[9].clone());
    Arc::make_mut(&mut forged.header).previous_block_hash = blocks[6].hash();
    let mut forged_rx =
        state.queue_and_commit_to_non_finalized_state(Arc::new(forged).prepare(), None, None);
    assert!(matches!(
        forged_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));

    // Write block 6, finish checkpoint sync, and switch with the honest block 9.
    assert!(
        write_sender.send(held_parent).is_ok(),
        "write task is running"
    );
    state.block_write_sender.finalized = Some(write_sender);
    parent_rx
        .blocking_recv()
        .expect("write task responds")
        .expect("block 6 commits");
    commit_checkpoint(&mut state, 7);
    commit_checkpoint(&mut state, 8);
    let _honest_rx =
        state.queue_and_commit_to_non_finalized_state(blocks[9].clone().prepare(), None, None);
    assert!(state.block_write_sender.finalized.is_none());

    let rejected = forged_rx
        .try_recv()
        .expect("forged child is answered at the switch")
        .expect_err("forged child is rejected");
    assert_eq!(rejected.inner().misbehavior_score(), 100);
    assert!(!state
        .non_finalized_state_queued_blocks
        .has_queued_children(blocks[6].hash()));

    Ok(())
}

/// An honest block queued before the final checkpoint commits isn't rejected, and commits after
/// the switch to full validation.
#[test]
fn honest_child_queued_before_final_checkpoint_commits() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let blocks: Vec<Arc<Block>> = network
        .blockchain_map()
        .range(0..=10)
        .map(|(_height, bytes)| bytes.zcash_deserialize_into().map(Arc::new))
        .collect::<Result<_, _>>()?;

    let runtime = Runtime::new()?;
    let (mut state, _, _, _) = runtime.block_on(StateService::new(
        Config::ephemeral(),
        &network,
        Height(8),
        0,
    ));

    for block in &blocks[..=7] {
        state
            .queue_and_commit_to_finalized_state(CheckpointVerifiedBlock::from(block.clone()), None)
            .blocking_recv()
            .expect("write task responds")
            .expect("checkpoint verified block commits");
    }

    // The first block above the final checkpoint arrives before the final checkpoint.
    let mut child_rx =
        state.queue_and_commit_to_non_finalized_state(blocks[9].clone().prepare(), None, None);
    assert!(matches!(
        child_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));

    state
        .queue_and_commit_to_finalized_state(CheckpointVerifiedBlock::from(blocks[8].clone()), None)
        .blocking_recv()
        .expect("write task responds")
        .expect("final checkpoint commits");
    assert!(matches!(
        child_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));

    // The next block triggers the switch, and both commit.
    let grandchild_rx =
        state.queue_and_commit_to_non_finalized_state(blocks[10].clone().prepare(), None, None);
    assert!(state.block_write_sender.finalized.is_none());
    assert_eq!(
        child_rx.blocking_recv().expect("child is answered")?,
        blocks[9].hash()
    );
    assert_eq!(
        grandchild_rx
            .blocking_recv()
            .expect("grandchild is answered")?,
        blocks[10].hash()
    );

    Ok(())
}

/// A same-hash replacement must commit its own body, not the first caller's authorizing data.
#[test]
fn queued_replacement_commits_its_own_body() -> Result<()> {
    use zebra_chain::{
        block::ChainHistoryBlockTxAuthCommitmentHash,
        primitives::zcash_history::BlockCommitmentTreeRoots, serialization::BytesInDisplayOrder,
    };

    let _init_guard = zebra_test::init();
    let ledger_strategy = nu5_ledger_strategy();

    proptest!(
        ProptestConfig::with_cases(1),
        |((chain, _count, network, _history_tree) in PreparedChain::default()
            .with_ledger_strategy(ledger_strategy)
            .with_valid_commitments()
            .no_shrink())| {
            let nu5_height = NetworkUpgrade::Nu5.activation_height(&network).unwrap();
            let parent_index = chain.iter().position(|block| block.height == nu5_height).unwrap();

            Runtime::new().unwrap().block_on(async {
                tokio::time::timeout(Duration::from_secs(30), async {
                    let (mut state, _, _, _) = StateService::new(
                        Config::ephemeral(),
                        &network,
                        chain[parent_index - 1].height,
                        0,
                    ).await;

                    for block in chain.iter().take(parent_index) {
                        state
                            .queue_and_commit_to_finalized_state(
                                CheckpointVerifiedBlock::from(block.block.clone()),
                                None,
                            )
                            .await
                            .expect("write task responds")
                            .expect("valid prefix commits");
                    }

                    // Transparent-only coinbases avoid arbitrary shielded anchors. Zero-valued
                    // outputs leave the generated value pools and note commitment trees unchanged.
                    let trees = state.read_service.db.note_commitment_trees_for_tip();
                    let mut history_tree = state.read_service.db.history_tree();
                    let mut previous_hash = state.read_service.db.finalized_tip_hash();
                    let mut next_block = |template: &SemanticallyVerifiedBlock| {
                        let coinbase = transaction::Transaction::test_v5(
                            NetworkUpgrade::Nu5,
                            template.block.transactions[0].inputs(),
                            vec![transparent::Output {
                                value: 0.try_into().unwrap(),
                                lock_script: transparent::Script::new(&[]),
                            }],
                            transaction::LockTime::unlocked(),
                            template.height,
                        );
                        let mut block = Block::clone(&template.block);
                        block.transactions = vec![Arc::new(coinbase)];
                        let commitment = ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
                            &history_tree.hash().expect("the prefix includes Heartwood"),
                            &block.auth_data_root(),
                        );
                        let header = Arc::make_mut(&mut block.header);
                        header.previous_block_hash = previous_hash;
                        header.merkle_root = block.transactions.iter().collect();
                        header.commitment_bytes = commitment.bytes_in_serialized_order().into();
                        let block = Arc::new(block);
                        Arc::make_mut(&mut history_tree).push(
                            &network,
                            block.clone(),
                            BlockCommitmentTreeRoots {
                                sapling: &trees.sapling.root(),
                                orchard: &trees.orchard.root(),
                                ironwood: &trees.ironwood.root(),
                            },
                        ).expect("transparent-only blocks extend the history tree");
                        previous_hash = block.hash();
                        block.prepare()
                    };
                    let parent = next_block(&chain[parent_index]);
                    let honest = next_block(&chain[parent_index + 1]);
                    let forged = coinbase_authorization_twin(&honest.block);
                    assert_eq!(forged.hash(), honest.hash);
                    assert_eq!(forged.transactions[0].hash(), honest.block.transactions[0].hash());
                    assert_ne!(forged.auth_data_root(), honest.block.auth_data_root());

                    // Both bodies reach the queue before their missing parent arrives.
                    let forged_received = Instant::now();
                    let mut forged = forged.prepare();
                    forged.received_time = Some(forged_received);
                    let mut honest = honest;
                    honest.received_time = Some(forged_received + Duration::from_secs(1));
                    let forged_rx = state.queue_and_commit_to_non_finalized_state(forged, None, None);
                    let honest_rx =
                        state.queue_and_commit_to_non_finalized_state(honest.clone(), None, None);
                    assert!(state.non_finalized_state_queued_blocks.has(honest.hash));

                    let replaced = forged_rx
                        .await
                        .expect("state responds")
                        .expect_err("the older caller is replaced");
                    assert_eq!(
                        replaced.inner(),
                        &CommitBlockError::new_duplicate(
                            Some(honest.hash.into()),
                            crate::KnownBlock::Queue,
                        ),
                    );

                    state
                        .queue_and_commit_to_non_finalized_state(parent, None, None)
                        .await
                        .expect("write task responds")
                        .expect("valid parent commits");
                    assert_eq!(
                        honest_rx
                            .await
                            .expect("write task responds")
                            .expect("honest replacement commits"),
                        honest.hash,
                    );
                    let non_finalized_state = state.read_service.latest_non_finalized_state();
                    let tip = non_finalized_state
                        .best_tip_block()
                        .expect("the replacement is committed");
                    assert_eq!(tip.block, honest.block);
                    // Blocks with the same hash compare equal, so also compare their authorizing data.
                    assert_eq!(tip.block.auth_data_root(), honest.block.auth_data_root());
                    assert_eq!(
                        tip.received_time, honest.received_time,
                        "a different body doesn't take the replaced body's receipt time",
                    );
                })
                .await
                .expect("queued replacement completes before the timeout");
            });
        }
    );

    Ok(())
}

/// A same-hash replacement must commit its own body, not the first caller's authorizing data.
#[test]
fn queued_forged_replacement_fails_and_scores_only_forger() -> Result<()> {
    use zebra_chain::{
        block::ChainHistoryBlockTxAuthCommitmentHash,
        primitives::zcash_history::BlockCommitmentTreeRoots, serialization::BytesInDisplayOrder,
    };

    let _init_guard = zebra_test::init();
    let ledger_strategy = nu5_ledger_strategy();

    proptest!(
        ProptestConfig::with_cases(1),
        |((chain, _count, network, _history_tree) in PreparedChain::default()
            .with_ledger_strategy(ledger_strategy)
            .with_valid_commitments()
            .no_shrink())| {
            let nu5_height = NetworkUpgrade::Nu5.activation_height(&network).unwrap();
            let parent_index = chain.iter().position(|block| block.height == nu5_height).unwrap();

            Runtime::new().unwrap().block_on(async {
                tokio::time::timeout(Duration::from_secs(30), async {
                    let (mut state, _, _, _) = StateService::new(
                        Config::ephemeral(),
                        &network,
                        chain[parent_index - 1].height,
                        0,
                    ).await;

                    for block in chain.iter().take(parent_index) {
                        state
                            .queue_and_commit_to_finalized_state(
                                CheckpointVerifiedBlock::from(block.block.clone()),
                                None,
                            )
                            .await
                            .expect("write task responds")
                            .expect("valid prefix commits");
                    }

                    // Transparent-only coinbases avoid arbitrary shielded anchors. Zero-valued
                    // outputs leave the generated value pools and note commitment trees unchanged.
                    let trees = state.read_service.db.note_commitment_trees_for_tip();
                    let mut history_tree = state.read_service.db.history_tree();
                    let mut previous_hash = state.read_service.db.finalized_tip_hash();
                    let mut next_block = |template: &SemanticallyVerifiedBlock| {
                        let coinbase = transaction::Transaction::test_v5(
                            NetworkUpgrade::Nu5,
                            template.block.transactions[0].inputs(),
                            vec![transparent::Output {
                                value: 0.try_into().unwrap(),
                                lock_script: transparent::Script::new(&[]),
                            }],
                            transaction::LockTime::unlocked(),
                            template.height,
                        );
                        let mut block = Block::clone(&template.block);
                        block.transactions = vec![Arc::new(coinbase)];
                        let commitment = ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
                            &history_tree.hash().expect("the prefix includes Heartwood"),
                            &block.auth_data_root(),
                        );
                        let header = Arc::make_mut(&mut block.header);
                        header.previous_block_hash = previous_hash;
                        header.merkle_root = block.transactions.iter().collect();
                        header.commitment_bytes = commitment.bytes_in_serialized_order().into();
                        let block = Arc::new(block);
                        Arc::make_mut(&mut history_tree).push(
                            &network,
                            block.clone(),
                            BlockCommitmentTreeRoots {
                                sapling: &trees.sapling.root(),
                                orchard: &trees.orchard.root(),
                                ironwood: &trees.ironwood.root(),
                            },
                        ).expect("transparent-only blocks extend the history tree");
                        previous_hash = block.hash();
                        block.prepare()
                    };
                    let parent = next_block(&chain[parent_index]);
                    let honest = next_block(&chain[parent_index + 1]);
                    let forged = coinbase_authorization_twin(&honest.block);
                    assert_eq!(forged.hash(), honest.hash);
                    assert_eq!(forged.transactions[0].hash(), honest.block.transactions[0].hash());
                    assert_ne!(forged.auth_data_root(), honest.block.auth_data_root());

                    // The honest body is queued first, the forged one replaces it.
                    let honest_rx =
                        state.queue_and_commit_to_non_finalized_state(honest.clone(), None, None);
                    let forged_rx =
                        state.queue_and_commit_to_non_finalized_state(forged.prepare(), None, None);

                    let replaced = honest_rx.await.expect("state responds").expect_err("replaced");
                    assert!(replaced.inner().is_duplicate_request());
                    assert_eq!(replaced.inner().misbehavior_score(), 0);

                    state
                        .queue_and_commit_to_non_finalized_state(parent, None, None)
                        .await
                        .expect("write task responds")
                        .expect("valid parent commits");
                    let forged_err = forged_rx.await.expect("write task responds").expect_err("forged fails");
                    assert_eq!(forged_err.inner().misbehavior_score(), 100);
                    assert!(!state.non_finalized_state_queued_blocks.has(honest.hash));
                    assert_ne!(
                        state.read_service.latest_non_finalized_state().best_tip_block().map(|b| b.hash),
                        Some(honest.hash),
                    );
                })
                .await
                .expect("queued replacement completes before the timeout");
            });
        }
    );

    Ok(())
}

/// A request for a hash already sent to the write task gets its own duplicate error and does not
/// take over another request's response channel.
#[tokio::test]
async fn write_channel_duplicate_keeps_its_own_channel() {
    let _init_guard = zebra_test::init();
    let (mut state, _, _, _) =
        StateService::new(Config::ephemeral(), &Network::Mainnet, Height::MAX, 0).await;

    let blocks: Vec<SemanticallyVerifiedBlock> = zebra_test::vectors::MAINNET_BLOCKS
        .range(2..=3)
        .map(|(_, bytes)| {
            let block: Arc<Block> = bytes.zcash_deserialize_into().unwrap();
            block.prepare()
        })
        .collect();

    // Block 3 is queued behind block 2, which has been sent to the write task.
    let mut queued_rx =
        state.queue_and_commit_to_non_finalized_state(blocks[1].clone(), None, None);
    state.non_finalized_block_write_sent_hashes.add(&blocks[0]);

    let duplicate = state
        .queue_and_commit_to_non_finalized_state(blocks[0].clone(), None, None)
        .await
        .expect("state responds")
        .expect_err("the hash was already sent to the write task");
    assert_eq!(
        duplicate.inner(),
        &CommitBlockError::new_duplicate(
            Some(blocks[0].hash.into()),
            crate::KnownBlock::WriteChannel,
        ),
    );

    assert!(
        matches!(
            queued_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ),
        "the queued request keeps waiting on its own channel"
    );
    assert!(state.non_finalized_state_queued_blocks.has(blocks[1].hash));
    assert!(!state.non_finalized_state_queued_blocks.has(blocks[0].hash));
}

/// Another copy of a queued body keeps the receipt time of its first copy, so an equal-work
/// sibling received between the two copies doesn't win `Chain::cmp`'s first-received tie-break.
#[tokio::test]
async fn queued_identical_replacement_keeps_earliest_receipt_time() {
    let _init_guard = zebra_test::init();

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_3_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let first_received = Instant::now();
    let later = first_received + Duration::from_secs(1);

    // Each copy is `(first copy's receipt time, second copy's, the queued block's)`.
    // Unstamped blocks count as received before any stamped block.
    for (first, second, expected) in [
        (Some(first_received), Some(later), Some(first_received)),
        (Some(later), Some(first_received), Some(first_received)),
        (None, Some(later), None),
    ] {
        let (mut state, _, _, _) =
            StateService::new(Config::ephemeral(), &Network::Mainnet, Height::MAX, 0).await;

        let mut first_copy = block.clone().prepare();
        first_copy.received_time = first;
        let mut second_copy = block.clone().prepare();
        second_copy.received_time = second;

        let first_rx = state.queue_and_commit_to_non_finalized_state(first_copy, None, None);
        let mut second_rx = state.queue_and_commit_to_non_finalized_state(second_copy, None, None);

        assert!(first_rx
            .await
            .expect("state responds")
            .expect_err("the older caller is replaced")
            .inner()
            .is_duplicate_request());
        assert!(
            matches!(
                second_rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ),
            "the newer caller keeps waiting on its own channel"
        );

        let (queued, _rsp_tx, _, _) = state
            .non_finalized_state_queued_blocks
            .remove_queued(&block.hash())
            .expect("the second copy is queued");
        assert_eq!(queued.received_time, expected);
    }
}

#[tokio::test]
async fn known_blocks_preserves_order_and_queue_locations() {
    let _init_guard = zebra_test::init();
    let (mut state, _, _, _) =
        StateService::new(Config::ephemeral(), &Network::Mainnet, Height::MAX, 0).await;
    let blocks: Vec<SemanticallyVerifiedBlock> = zebra_test::vectors::MAINNET_BLOCKS
        .range(1..=3)
        .map(|(_, bytes)| {
            let block: Arc<Block> = bytes.zcash_deserialize_into().unwrap();
            block.prepare()
        })
        .collect();
    let (sender, _receiver) = tokio::sync::oneshot::channel();
    state
        .non_finalized_state_queued_blocks
        .queue((blocks[0].clone(), sender, None, None));
    state.non_finalized_block_write_sent_hashes.add(&blocks[1]);
    let hashes = vec![
        blocks[1].hash,
        blocks[2].hash,
        blocks[0].hash,
        blocks[1].hash,
    ];
    let mut expected = Vec::new();
    for hash in &hashes {
        let Response::KnownBlock(location) = state.call(Request::KnownBlock(*hash)).await.unwrap()
        else {
            unreachable!("KnownBlock returns its matching response");
        };
        expected.push(location);
    }
    assert_eq!(
        expected,
        vec![
            Some(crate::KnownBlock::WriteChannel),
            None,
            Some(crate::KnownBlock::Queue),
            Some(crate::KnownBlock::WriteChannel),
        ]
    );
    assert_eq!(
        state.call(Request::KnownBlocks(hashes)).await.unwrap(),
        Response::KnownBlocks(expected)
    );
    assert_eq!(
        state.call(Request::KnownBlocks(Vec::new())).await.unwrap(),
        Response::KnownBlocks(Vec::new())
    );
}

/// Start the real writer with a finalized genesis, leaving block 1 absent for orphan queue tests.
async fn mined_notification_test_state() -> (StateService, Arc<Block>) {
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

#[tokio::test(flavor = "multi_thread")]
async fn mined_queued_commit_survives_cancellation_and_network_replacement() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut state, parent) = mined_notification_test_state().await;
        let child = parent.make_fake_child();
        let (committed, mut commits) = tokio::sync::mpsc::channel(1);
        let (validated, bodies) = tokio::sync::watch::channel(None);
        let response = state.queue_and_commit_to_non_finalized_state(
            child.clone().prepare(),
            None,
            Some(MinedBlockNotifications::new(committed.clone(), validated)),
        );
        assert_eq!(
            committed.capacity(),
            1,
            "orphans must not reserve relay capacity"
        );
        drop(response);
        // A network replacement must not discard the accepted mined notification.
        drop(state.queue_and_commit_to_non_finalized_state(child.clone().prepare(), None, None));
        assert!(commits.try_recv().is_err());
        state
            .queue_and_commit_to_non_finalized_state(parent.prepare(), None, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(commits.recv().await, Some((child.hash(), Height(2))));
        assert!(state
            .read_service
            .latest_non_finalized_state()
            .any_chain_contains(&child.hash()));
        assert!(
            bodies.borrow().is_none(),
            "semantic misses must not relay early"
        );
    })
    .await
    .expect("queued mined commit must finish after its parent arrives");
}

#[tokio::test(flavor = "multi_thread")]
async fn mined_notifications_bound_admission_without_stalling_ordinary_commits() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut state, parent) = mined_notification_test_state().await;
        let best = parent.make_fake_child().set_work(100);
        let best_tip = best.make_fake_child();
        for block in [&parent, &best, &best_tip] {
            state
                .queue_and_commit_to_non_finalized_state(block.clone().prepare(), None, None)
                .await
                .unwrap()
                .unwrap();
        }
        let side = parent.make_fake_child().set_work(50);
        let mut proposal_state = state.read_service.latest_non_finalized_state();
        proposal_state.disable_metrics();
        proposal_state
            .commit_block(side.clone().prepare(), &state.read_service.db)
            .unwrap();
        let contextual = proposal_state
            .find_chain(|chain| chain.non_finalized_tip_hash() == side.hash())
            .unwrap()
            .tip_block()
            .unwrap()
            .clone();
        let (committed, mut commits) = tokio::sync::mpsc::channel(1);
        let (validated, mut bodies) = tokio::sync::watch::channel(None);
        let response = state.queue_and_commit_to_non_finalized_state(
            side.clone().prepare(),
            Some(Box::new(contextual)),
            Some(MinedBlockNotifications::new(
                committed.clone(),
                validated.clone(),
            )),
        );
        bodies.changed().await.unwrap();
        assert_eq!(*bodies.borrow_and_update(), Some(side.clone()));
        assert_eq!(response.await.unwrap().unwrap(), side.hash());
        assert_eq!(commits.try_recv().unwrap(), (side.hash(), Height(2)));
        assert_eq!(state.best_tip(), Some((Height(3), best_tip.hash())));

        let next = best_tip.make_fake_child();
        let mut proposal_state = state.read_service.latest_non_finalized_state();
        proposal_state.disable_metrics();
        proposal_state
            .commit_block(next.clone().prepare(), &state.read_service.db)
            .unwrap();
        let contextual = proposal_state.best_tip_block().unwrap().clone();
        committed.try_send((parent.hash(), Height(1))).unwrap();
        let error = state
            .queue_and_commit_to_non_finalized_state(
                next.clone().prepare(),
                Some(Box::new(contextual.clone())),
                Some(MinedBlockNotifications::new(
                    committed.clone(),
                    validated.clone(),
                )),
            )
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error.inner(),
            &crate::CommitBlockError::MinedBlockNotificationFull
        );
        assert_eq!(error.inner().misbehavior_score(), 0);
        assert!(!state
            .read_service
            .latest_non_finalized_state()
            .any_chain_contains(&next.hash()));
        assert!(
            !bodies.has_changed().unwrap(),
            "full channels must reject before early relay"
        );
        assert_eq!(commits.try_recv().unwrap(), (parent.hash(), Height(1)));

        drop(commits);
        let error = state
            .queue_and_commit_to_non_finalized_state(
                next.clone().prepare(),
                Some(Box::new(contextual)),
                Some(MinedBlockNotifications::new(committed, validated.clone())),
            )
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error.inner(),
            &crate::CommitBlockError::MinedBlockNotificationClosed
        );
        assert_eq!(error.inner().misbehavior_score(), 0);
        assert!(!state
            .read_service
            .latest_non_finalized_state()
            .any_chain_contains(&next.hash()));
        assert!(
            !bodies.has_changed().unwrap(),
            "closed channels must reject before early relay"
        );
        assert_eq!(
            state
                .queue_and_commit_to_non_finalized_state(next.clone().prepare(), None, None)
                .await
                .unwrap()
                .unwrap(),
            next.hash(),
        );
    })
    .await
    .expect("full or closed notification channels must not stall ordinary commits");
}

#[tokio::test(flavor = "multi_thread")]
async fn mined_rejections_and_stale_parent_publish_no_notifications() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut state, root) = mined_notification_test_state().await;
        let parent = root.make_fake_child();
        for block in [&root, &parent] {
            state
                .queue_and_commit_to_non_finalized_state(block.clone().prepare(), None, None)
                .await
                .unwrap()
                .unwrap();
        }
        let child = parent.make_fake_child();
        let mut proposal_state = state.read_service.latest_non_finalized_state();
        proposal_state.disable_metrics();
        proposal_state
            .commit_block(child.clone().prepare(), &state.read_service.db)
            .unwrap();
        let contextual = proposal_state.best_tip_block().unwrap().clone();
        let (committed, mut commits) = tokio::sync::mpsc::channel(1);
        let (validated, bodies) = tokio::sync::watch::channel(None);
        let notifications = MinedBlockNotifications::new(committed, validated);
        state
            .queue_and_commit_to_non_finalized_state(child.clone().prepare(), None, None)
            .await
            .unwrap()
            .unwrap();
        state
            .send_invalidate_block(child.hash())
            .await
            .unwrap()
            .unwrap();
        // Exercise the actual writer after admission, bypassing only the duplicate sent-hash guard.
        let (response_sender, response) = tokio::sync::oneshot::channel();
        state
            .block_write_sender
            .non_finalized
            .as_ref()
            .unwrap()
            .send(super::write::NonFinalizedWriteMessage::Commit(
                (
                    child.clone().prepare(),
                    response_sender,
                    Some(Box::new(contextual.clone())),
                    Some(notifications.clone()),
                ),
                std::time::Instant::now(),
            ))
            .unwrap_or_else(|_| panic!("the state writer is still running"));
        assert!(response.await.unwrap().is_err());
        assert!(bodies.borrow().is_none());
        assert!(commits.try_recv().is_err());
        state
            .send_invalidate_block(parent.hash())
            .await
            .unwrap()
            .unwrap();
        // The queue still knows the sent parent, but the live writer must resolve it again.
        assert!(state
            .queue_and_commit_to_non_finalized_state(
                child.clone().prepare(),
                Some(Box::new(contextual)),
                Some(notifications.clone()),
            )
            .await
            .unwrap()
            .is_err());
        assert!(bodies.borrow().is_none());
        assert!(commits.try_recv().is_err());

        let mut rejected = root.make_fake_child().set_work(50).prepare();
        rejected.height = Height(3);
        assert!(state
            .queue_and_commit_to_non_finalized_state(rejected, None, Some(notifications),)
            .await
            .unwrap()
            .is_err());
        assert!(bodies.borrow().is_none());
        assert!(commits.try_recv().is_err());
    })
    .await
    .expect("rejected mined blocks must not wait for relay capacity");
}

#[tokio::test(flavor = "multi_thread")]
async fn mined_checkpoint_notification_survives_cancellation_and_replacement() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut state, _, _, _) =
            StateService::new(Config::ephemeral(), &Network::Mainnet, Height::MAX, 0).await;
        let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .unwrap();
        let child = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .unwrap();
        let (committed, mut commits) = tokio::sync::mpsc::channel(1);
        let (validated, bodies) = tokio::sync::watch::channel(None);
        drop(state.call(Request::CommitMinedCheckpointVerifiedBlock {
            block: child.clone().into(),
            notifications: MinedBlockNotifications::new(committed.clone(), validated),
        }));
        assert_eq!(committed.capacity(), 1);
        drop(state.call(Request::CommitCheckpointVerifiedBlock(child.clone().into())));
        state
            .call(Request::CommitCheckpointVerifiedBlock(genesis.into()))
            .await
            .unwrap();
        assert_eq!(commits.recv().await, Some((child.hash(), Height(1))));
        assert_eq!(state.read_service.db.finalized_tip_hash(), child.hash());
        assert!(bodies.borrow().is_none());
    })
    .await
    .expect("checkpoint notifications must survive caller cancellation");
}

#[tokio::test(flavor = "multi_thread")]
async fn mined_checkpoint_admission_errors_reset_before_reply() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut state, _, _, _) =
            StateService::new(Config::ephemeral(), &Network::Mainnet, Height::MAX, 0).await;
        let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .unwrap();
        let (committed, mut commits) = tokio::sync::mpsc::channel(1);
        let (validated, bodies) = tokio::sync::watch::channel(None);
        committed.try_send((genesis.hash(), Height(0))).unwrap();
        let error = state
            .call(Request::CommitMinedCheckpointVerifiedBlock {
                block: genesis.clone().into(),
                notifications: MinedBlockNotifications::new(committed.clone(), validated.clone()),
            })
            .await
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::CommitCheckpointVerifiedError>()
                .unwrap()
                .inner(),
            &crate::CommitBlockError::MinedBlockNotificationFull,
        );
        assert_eq!(state.read_service.db.finalized_tip_height(), None);
        assert_eq!(commits.try_recv().unwrap(), (genesis.hash(), Height(0)));
        drop(commits);
        let error = state
            .call(Request::CommitMinedCheckpointVerifiedBlock {
                block: genesis.clone().into(),
                notifications: MinedBlockNotifications::new(committed, validated),
            })
            .await
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::CommitCheckpointVerifiedError>()
                .unwrap()
                .inner(),
            &crate::CommitBlockError::MinedBlockNotificationClosed,
        );
        assert_eq!(state.read_service.db.finalized_tip_height(), None);
        assert!(bodies.borrow().is_none());
        // Both failure responses must leave the checkpoint queue ready for an immediate retry.
        state
            .call(Request::CommitCheckpointVerifiedBlock(genesis.into()))
            .await
            .unwrap();
        assert_eq!(
            state.read_service.db.finalized_tip_height(),
            Some(Height(0))
        );
    })
    .await
    .expect("checkpoint admission failures must not strand subsequent ordinary commits");
}

#[tokio::test(flavor = "multi_thread")]
async fn mined_cached_future_time_suppresses_early_body_but_preserves_commit() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut state, parent) = mined_notification_test_state().await;
        state
            .queue_and_commit_to_non_finalized_state(parent.clone().prepare(), None, None)
            .await
            .unwrap()
            .unwrap();
        // Model a solve checked with an ahead-of-current wall clock, without changing global time.
        let checked_at = chrono::Utc::now() + chrono::Duration::days(1);
        let mut child = parent.make_fake_child();
        Arc::make_mut(&mut Arc::make_mut(&mut child).header).time = checked_at;
        let height = child.coinbase_height().unwrap();
        let hash = child.hash();
        child
            .header
            .time_is_valid_at(checked_at, &height, &hash)
            .unwrap();
        assert!(child
            .header
            .time_is_valid_at(chrono::Utc::now(), &height, &hash)
            .is_err());
        let mut proposal_state = state.read_service.latest_non_finalized_state();
        proposal_state.disable_metrics();
        proposal_state
            .commit_block(child.clone().prepare(), &state.read_service.db)
            .unwrap();
        let contextual = proposal_state.best_tip_block().unwrap().clone();
        let (committed, mut commits) = tokio::sync::mpsc::channel(1);
        let (validated, bodies) = tokio::sync::watch::channel(None);
        assert_eq!(
            state
                .queue_and_commit_to_non_finalized_state(
                    child.prepare(),
                    Some(Box::new(contextual)),
                    Some(MinedBlockNotifications::new(committed, validated)),
                )
                .await
                .unwrap()
                .unwrap(),
            hash,
        );
        assert!(
            bodies.borrow().is_none(),
            "the writer must recheck time before early relay"
        );
        assert_eq!(commits.try_recv().unwrap(), (hash, height));
        assert_eq!(state.best_tip(), Some((height, hash)));
    })
    .await
    .expect("a live time check must not change normal cached commit handling");
}
