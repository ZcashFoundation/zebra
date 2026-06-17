//! StateService test vectors.

#![allow(clippy::unwrap_in_result)]

// TODO: move these tests into tests::vectors and tests::prop modules.

use std::{env, sync::Arc, time::Duration};

use tokio::runtime::Runtime;
use tower::{buffer::Buffer, util::BoxService, Service, ServiceExt};

use zebra_chain::{
    block::{self, Block, CountedHeader, Height},
    chain_tip::ChainTip,
    fmt::SummaryDebug,
    parameters::{Network, NetworkUpgrade},
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    transaction, transparent,
    value_balance::ValueBalance,
};

use zebra_test::{prelude::*, transcript::Transcript};

use crate::{
    arbitrary::Prepare,
    init_test,
    service::{
        arbitrary::populated_state, chain_tip::TipAction, headers_by_height_range,
        non_finalized_state::Chain, StateService,
    },
    tests::{
        setup::{partial_nu5_chain_strategy, transaction_v4_from_coinbase},
        FakeChainHelper,
    },
    BoxError, CheckpointVerifiedBlock, Config, ReadRequest, ReadResponse, Request, Response,
    SemanticallyVerifiedBlock,
};

const LAST_BLOCK_HEIGHT: u32 = 10;

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

/// Regression test for the checkpoint-to-non-finalized sync handoff stall.
///
/// The block write task switches from committing checkpoint verified blocks (finalized state) to
/// committing semantically verified blocks (non-finalized state) once the final checkpoint block is
/// durably written to disk. That handoff used to also require a semantically verified child to be
/// queued, so the pipeline could stall at the checkpoint boundary until the first fully-verified
/// block arrived (or the syncer restarted).
///
/// This test sets the maximum checkpoint height to the last finalized block, commits the checkpoint
/// blocks, and asserts that `poll_ready()` performs the handoff with an **empty** non-finalized
/// queue — i.e. it no longer waits for a semantically verified block to arrive.
///
/// It deliberately does not commit a semantically verified block afterwards: the first two Mainnet
/// blocks predate the Canopy checkpoint, and the non-finalized write path treats their transaction
/// versions as `unreachable!()` (pre-Canopy blocks are only ever checkpoint verified). The handoff
/// itself is what this test covers.
#[tokio::test(flavor = "multi_thread")]
async fn poll_ready_hands_off_at_max_checkpoint_height() -> Result<()> {
    use std::task::{Context, Waker};

    use tower::Service;

    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    // Blocks 0 and 1 are committed as checkpoint verified (finalized) blocks.
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::MAINNET_BLOCKS
        .range(0..=1)
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into::<Arc<Block>>().unwrap())
        .collect();

    // Set the maximum checkpoint height to block 1, so the checkpoint phase ends once block 1 is
    // committed to the finalized state.
    let max_checkpoint_height = blocks[1].coinbase_height().unwrap();
    let (mut state_service, _read, _tip, _tip_change) =
        StateService::new(Config::ephemeral(), &network, max_checkpoint_height, 0).await;

    // Commit blocks 0 and 1 to the finalized state and wait for each write to land on disk, so the
    // finalized tip catches up to the maximum checkpoint height and the last block hash we sent.
    for block in &blocks[0..=1] {
        let checkpoint = CheckpointVerifiedBlock::from(block.clone());
        let result = state_service
            .queue_and_commit_to_finalized_state(checkpoint)
            .await;
        assert!(
            matches!(result, Ok(Ok(_))),
            "checkpoint verified block should commit: {result:?}",
        );
    }

    let last_finalized_hash = blocks[1].hash();
    assert_eq!(
        state_service.read_service.db.finalized_tip_height(),
        Some(max_checkpoint_height),
        "finalized tip should have reached the maximum checkpoint height",
    );
    assert_eq!(
        state_service.read_service.db.finalized_tip_hash(),
        last_finalized_hash,
        "finalized tip on disk should have caught up to block 1",
    );

    // Preconditions: still in finalized-write mode, and crucially **no** semantically verified block
    // is queued. The old behavior would not hand off in this state.
    assert!(
        state_service.block_write_sender.finalized.is_some(),
        "write task should still be committing finalized blocks before the handoff",
    );
    assert!(
        !state_service
            .non_finalized_state_queued_blocks
            .has_queued_children(last_finalized_hash),
        "no semantically verified block should be queued before the handoff",
    );

    // Trigger the handoff. Nothing is queued, so this exercises the height-based path.
    let mut cx = Context::from_waker(Waker::noop());
    let _ = state_service.poll_ready(&mut cx);

    // The handoff should have happened purely because the final checkpoint write is durable, with no
    // semantically verified block queued. Before this fix, the finalized sender would still be open.
    assert!(
        state_service.block_write_sender.finalized.is_none(),
        "poll_ready should have handed off to non-finalized writes at the max checkpoint height",
    );

    Ok(())
}

/// Micro-benchmark for the cost added to `poll_ready()` by the handoff trigger.
///
/// `poll_ready()` runs on essentially every state service readiness poll, so the added
/// `try_handoff_to_non_finalized_write()` call must be cheap. This measures three regimes:
///
/// - Raw `finalized_tip_hash()` DB read — a RocksDB seek-to-last. During checkpoint sync the
///   last-sent hash usually runs ahead of the on-disk tip, so the helper short-circuits after this
///   single read; it is the dominant per-poll cost in that phase.
/// - Full guard (still in finalized mode, on-disk tip matches the last-sent hash but below the max
///   checkpoint height and with no queued child): the helper runs the whole condition — two tip
///   reads plus a `HashMap::contains_key` — without transitioning. This is the most expensive
///   non-transitioning path, hit only at the checkpoint boundary.
/// - Steady state (after the handoff, `block_write_sender.finalized == None`): the call
///   short-circuits on a single `Option::is_some()` check and never touches the database. This is
///   what runs for the entire post-sync life of the node.
///
/// Run with:
/// `cargo test -p zebra-state --release -- --ignored --nocapture handoff_trigger_microbench`
#[ignore]
#[allow(clippy::print_stdout)]
#[tokio::test(flavor = "multi_thread")]
async fn handoff_trigger_microbench() -> Result<()> {
    use std::time::Instant;

    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    let blocks: Vec<Arc<Block>> = zebra_test::vectors::MAINNET_BLOCKS
        .range(0..=1)
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into::<Arc<Block>>().unwrap())
        .collect();

    // Use `Height::MAX` so the height condition is never met: the helper runs its full guard but
    // never transitions, which is exactly the non-transitioning path we want to measure.
    let (mut state_service, _read, _tip, _tip_change) =
        StateService::new(Config::ephemeral(), &network, Height::MAX, 0).await;

    for block in &blocks[0..=1] {
        let checkpoint = CheckpointVerifiedBlock::from(block.clone());
        state_service
            .queue_and_commit_to_finalized_state(checkpoint)
            .await
            .expect("commit channel open")
            .expect("checkpoint block commits");
    }

    const ITERS: u32 = 1_000_000;

    // Regime 1: raw `finalized_tip_hash()` DB read.
    let start = Instant::now();
    for _ in 0..ITERS {
        std::hint::black_box(state_service.read_service.db.finalized_tip_hash());
    }
    let tip_ns = start.elapsed().as_nanos() as f64 / f64::from(ITERS);

    // Regime 2: full guard cost. The on-disk tip equals the last-sent hash, the height condition is
    // false (`Height::MAX`), and no child is queued, so the helper evaluates every condition but
    // does not transition.
    let start = Instant::now();
    for _ in 0..ITERS {
        std::hint::black_box(state_service.try_handoff_to_non_finalized_write());
    }
    let guard_ns = start.elapsed().as_nanos() as f64 / f64::from(ITERS);
    assert!(
        state_service.block_write_sender.finalized.is_some(),
        "the benchmark must not transition: finalized sender should still be open",
    );

    // Regime 3: steady-state cost (post-handoff). Drop the finalized sender so the helper
    // short-circuits immediately, exactly as it does for the rest of the node's life.
    state_service.block_write_sender.finalized = None;
    let start = Instant::now();
    for _ in 0..ITERS {
        std::hint::black_box(state_service.try_handoff_to_non_finalized_write());
    }
    let steady_ns = start.elapsed().as_nanos() as f64 / f64::from(ITERS);

    println!("handoff trigger micro-benchmark ({ITERS} iters each):");
    println!("  finalized_tip_hash() DB read : {tip_ns:>8.2} ns/call");
    println!("  helper, full guard           : {guard_ns:>8.2} ns/call");
    println!("  helper, steady state         : {steady_ns:>8.2} ns/call");

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn header_only_service_requests_preserve_body_boundary() -> std::result::Result<(), BoxError>
{
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let (mut state_service, read_state, _, _) =
        StateService::new(Config::ephemeral(), &network, Height::MAX, 0).await;
    let genesis =
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.zcash_deserialize_into::<Arc<Block>>()?;
    let block1 =
        zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into::<Arc<Block>>()?;
    let block2 =
        zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into::<Arc<Block>>()?;
    let block1_hash = block1.hash();
    let block2_hash = block2.hash();

    assert_eq!(
        state_service
            .ready()
            .await?
            .call(Request::CommitCheckpointVerifiedBlock(
                CheckpointVerifiedBlock::from(genesis.clone()),
            ))
            .await?,
        Response::Committed(genesis.hash()),
    );

    state_service.block_write_sender.finalized = None;
    let state = Buffer::new(BoxService::new(state_service), 1);

    assert_eq!(
        read_state
            .clone()
            .oneshot(ReadRequest::FinalizedTip)
            .await?,
        ReadResponse::FinalizedTip(Some((Height(0), genesis.hash()))),
    );

    let genesis_size = u32::try_from(genesis.zcash_serialize_to_vec()?.len())
        .expect("serialized block size fits in u32");
    assert_eq!(
        read_state
            .clone()
            .oneshot(ReadRequest::BlocksByHeightRange {
                start: Height(0),
                count: 3,
            })
            .await?,
        ReadResponse::Blocks(vec![(
            Height(0),
            genesis.clone(),
            genesis.zcash_serialize_to_vec()?.len(),
        )]),
    );

    assert_eq!(
        read_state
            .clone()
            .oneshot(ReadRequest::BlockSizeHints {
                from: Height(0),
                count: 1,
            })
            .await?,
        ReadResponse::BlockSizeHints(vec![(Height(0), Some(genesis_size))]),
    );

    assert_eq!(
        state
            .clone()
            .oneshot(Request::CommitHeaderRange {
                anchor: genesis.hash(),
                headers: vec![block1.header.clone(), block2.header.clone()],
                body_sizes: vec![999_999, 0],
            })
            .await?,
        Response::Committed(block2_hash),
    );

    assert_eq!(
        read_state
            .clone()
            .oneshot(ReadRequest::BlockSizeHints {
                from: Height(1),
                count: 2,
            })
            .await?,
        ReadResponse::BlockSizeHints(vec![(Height(1), Some(999_999)), (Height(2), None)]),
    );

    assert_eq!(
        state.clone().oneshot(Request::Depth(block1_hash)).await?,
        Response::Depth(None),
    );
    assert_eq!(
        read_state
            .clone()
            .oneshot(ReadRequest::Depth(block1_hash))
            .await?,
        ReadResponse::Depth(None),
    );
    assert_eq!(
        state
            .clone()
            .oneshot(Request::KnownBlock(block1_hash))
            .await?,
        Response::KnownBlock(None),
    );
    assert_eq!(
        state
            .clone()
            .oneshot(Request::KnownBlock(block2_hash))
            .await?,
        Response::KnownBlock(None),
    );
    assert_eq!(
        state
            .clone()
            .oneshot(Request::Block(Height(1).into()))
            .await?,
        Response::Block(None),
    );
    assert_eq!(
        state
            .clone()
            .oneshot(Request::Block(Height(2).into()))
            .await?,
        Response::Block(None),
    );
    assert_eq!(
        state
            .clone()
            .oneshot(Request::AnyChainBlock(block1_hash.into()))
            .await?,
        Response::Block(None),
    );

    assert_eq!(
        read_state
            .clone()
            .oneshot(ReadRequest::BestChainBlockHash(Height(1)))
            .await?,
        ReadResponse::BlockHash(None),
    );
    assert_eq!(
        read_state
            .clone()
            .oneshot(ReadRequest::TransactionIdsForBlock(Height(1).into()))
            .await?,
        ReadResponse::TransactionIdsForBlock(None),
    );
    assert_eq!(
        read_state
            .clone()
            .oneshot(ReadRequest::HeadersByHeightRange {
                start: Height(1),
                count: 2,
            })
            .await?,
        ReadResponse::Headers(vec![
            (Height(1), block1_hash, block1.header.clone()),
            (Height(2), block2_hash, block2.header.clone()),
        ]),
    );
    assert_eq!(
        read_state
            .clone()
            .oneshot(ReadRequest::BlocksByHeightRange {
                start: Height(1),
                count: 2,
            })
            .await?,
        ReadResponse::Blocks(Vec::new()),
    );

    assert_eq!(
        read_state
            .clone()
            .oneshot(ReadRequest::BestHeaderTip)
            .await?,
        ReadResponse::BestHeaderTip(Some((Height(2), block2_hash))),
    );
    assert_eq!(
        read_state
            .clone()
            .oneshot(ReadRequest::MissingBlockBodies {
                from: Height(1),
                limit: 10,
            })
            .await?,
        ReadResponse::MissingBlockBodies(vec![Height(1), Height(2)]),
    );

    assert_eq!(
        read_state.oneshot(ReadRequest::FinalizedTip).await?,
        ReadResponse::FinalizedTip(Some((Height(0), genesis.hash()))),
    );

    Ok(())
}

/// A node still in the finalized (checkpoint) write phase must be able to commit
/// a Zakura header range.
///
/// This reproduces the Zakura catch-up deadlock. A freshly started node has an
/// empty non-finalized chain set, so it keeps its finalized block-write sender
/// and the block write task drains the finalized channel before handling any
/// non-finalized message. The finalized->non-finalized transition only fires
/// when a non-finalized block is queued as a child of the finalized tip (the
/// legacy commit path). A node catching up to a peer that sits at a *static*
/// tip over Zakura commits header ranges via `CommitHeaderRange` (a
/// non-finalized message) but never queues such a block, so before the fix the
/// request never completes: the header tip stays at genesis, block sync stays
/// gated off (`best_header_tip <= verified_block_tip`), and the node stalls.
///
/// Unlike `header_only_service_requests_preserve_body_boundary`, this test does
/// NOT drop `block_write_sender.finalized`, so it exercises the real catch-up
/// state. Without the fix the `CommitHeaderRange` future never resolves and the
/// bounded wait below fails the test.
#[tokio::test(flavor = "multi_thread")]
async fn commit_header_range_completes_while_in_finalized_write_phase(
) -> std::result::Result<(), BoxError> {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let (mut state_service, read_state, _, _) =
        StateService::new(Config::ephemeral(), &network, Height::MAX, 0).await;
    let genesis =
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.zcash_deserialize_into::<Arc<Block>>()?;
    let block1 =
        zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into::<Arc<Block>>()?;
    let block2 =
        zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into::<Arc<Block>>()?;
    let block2_hash = block2.hash();

    assert_eq!(
        state_service
            .ready()
            .await?
            .call(Request::CommitCheckpointVerifiedBlock(
                CheckpointVerifiedBlock::from(genesis.clone()),
            ))
            .await?,
        Response::Committed(genesis.hash()),
    );

    // The node is still in the finalized write phase: committing a checkpoint
    // block does not trigger the finalized->non-finalized transition, which is
    // exactly the state a Zakura node catching up to a static tip is stuck in.
    assert!(
        state_service.block_write_sender.finalized.is_some(),
        "a fresh node stays in the finalized write phase after a checkpoint commit",
    );
    let state = Buffer::new(BoxService::new(state_service), 1);

    let committed = tokio::time::timeout(
        Duration::from_secs(20),
        state.clone().oneshot(Request::CommitHeaderRange {
            anchor: genesis.hash(),
            headers: vec![block1.header.clone(), block2.header.clone()],
            body_sizes: vec![999_999, 0],
        }),
    )
    .await
    .expect("CommitHeaderRange must not deadlock while in the finalized write phase")?;

    assert_eq!(committed, Response::Committed(block2_hash));

    assert_eq!(
        state
            .clone()
            .oneshot(Request::CommitCheckpointVerifiedBlock(
                CheckpointVerifiedBlock::from(block1.clone()),
            ))
            .await?,
        Response::Committed(block1.hash()),
    );

    assert_eq!(
        read_state.oneshot(ReadRequest::FinalizedTip).await?,
        ReadResponse::FinalizedTip(Some((Height(1), block1.hash()))),
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn header_range_reads_include_non_finalized_best_chain_blocks() -> Result<()> {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let (state_service, _read_state, _, _) =
        StateService::new(Config::ephemeral(), &network, Height::MAX, 0).await;
    let block1 = Arc::new(
        network
            .test_block(653599, 583999)
            .expect("fake test block can be built for a post-Canopy height"),
    );
    let block2 = block1.make_fake_child();
    let start = block1.coinbase_height().unwrap();
    let block1_hash = block1.hash();
    let block2_hash = block2.hash();
    let mut chain = Chain::new(
        &network,
        (start - 1).unwrap(),
        Default::default(),
        Default::default(),
        Default::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    );
    chain = chain.push(block1.clone().prepare().test_with_zero_spent_utxos())?;
    chain = chain.push(block2.clone().prepare().test_with_zero_spent_utxos())?;

    assert_eq!(
        headers_by_height_range(
            Some(Arc::new(chain)),
            &state_service.read_service.db,
            start,
            2,
        ),
        vec![
            (start, block1_hash, block1.header.clone()),
            (start.next().unwrap(), block2_hash, block2.header.clone()),
        ],
    );
    assert_eq!(
        headers_by_height_range(None::<Arc<Chain>>, &state_service.read_service.db, start, 2),
        Vec::new(),
    );

    Ok(())
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
        (network, nu_activation_height, chain) in partial_nu5_chain_strategy(4, true, UNDER_LEGACY_CHAIN_LIMIT, NetworkUpgrade::Canopy)
    ) {
        let response = crate::service::check::legacy_chain(nu_activation_height, chain.into_iter().rev(), &network, TEST_LEGACY_CHAIN_LIMIT)
            .map_err(|error| error.to_string());

        prop_assert_eq!(response, Ok(()));
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

        let response = crate::service::check::legacy_chain(nu_activation_height, chain.into_iter().rev(), &network, TEST_LEGACY_CHAIN_LIMIT)
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
        (network, nu_activation_height, chain) in partial_nu5_chain_strategy(5, false, OVER_LEGACY_CHAIN_LIMIT, NetworkUpgrade::Canopy)
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
            chain.clone().into_iter().rev(),
            &network,
            TEST_LEGACY_CHAIN_LIMIT,
        ).map_err(|error| error.to_string());

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
        (network, nu_activation_height, chain) in partial_nu5_chain_strategy(5, true, UNDER_LEGACY_CHAIN_LIMIT, NetworkUpgrade::Canopy)
    ) {
        let response = crate::service::check::legacy_chain(nu_activation_height, chain.into_iter().rev(), &network, TEST_LEGACY_CHAIN_LIMIT)
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
                let block_value_pool = &block.block.chain_value_pool_change(utxos, None)?;
                expected_finalized_value_pool += *block_value_pool;
            }

            let result_receiver = state_service.queue_and_commit_to_finalized_state(block.clone());
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
            let utxos = block.new_outputs.clone();
            let block_value_pool = &block.block.chain_value_pool_change(&transparent::utxos_from_ordered_utxos(utxos), None)?;
            expected_non_finalized_value_pool += *block_value_pool;

            let result_receiver = state_service.queue_and_commit_to_non_finalized_state(block.clone());
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

// This test sleeps for every block, so we only ever want to run it once
proptest! {
    #![proptest_config(
        proptest::test_runner::Config::with_cases(1)
    )]

    /// Test that the best tip height is updated accordingly.
    ///
    /// 1. Generate a finalized chain and some non-finalized blocks.
    /// 2. Check that initially the best tip height is empty.
    /// 3. Commit the finalized blocks and check that the best tip height is updated accordingly.
    /// 4. Commit the non-finalized blocks and check that the best tip height is also updated
    ///    accordingly.
    #[test]
    fn chain_tip_sender_is_updated(
        (network, finalized_blocks, non_finalized_blocks)
            in continuous_empty_blocks_from_test_vectors(),
    ) {
        let _init_guard = zebra_test::init();

        let (mut state_service, _read_only_state_service, latest_chain_tip, mut chain_tip_change) = Runtime::new().unwrap().block_on(async {
            // We're waiting to verify each block here, so we don't need the maximum checkpoint height.
            StateService::new(Config::ephemeral(), &network, Height::MAX, 0).await
        });

        prop_assert_eq!(latest_chain_tip.best_tip_height(), None);
        prop_assert_eq!(chain_tip_change.last_tip_change(), None);

        for block in finalized_blocks {
            let expected_block = block.clone();

            let expected_action = if expected_block.height == block::Height(0) {
                // Height 0 is reset by initialization. The BeforeOverwinter upgrade
                // (activation height 1) also resets at height 0 rather than at height 1,
                // because `ChainTipChange` resets one block *before* an activation height
                // (it checks `height.next()`, matching the height the mempool verifies
                // against). See `ChainTipChange::action`.
                TipAction::reset_with(expected_block.clone().into())
            } else {
                TipAction::grow_with(expected_block.clone().into())
            };

            let result_receiver = state_service.queue_and_commit_to_finalized_state(block);
            let result = result_receiver.blocking_recv();

            prop_assert!(result.is_ok(), "unexpected failed finalized block commit: {:?}", result);

            // Wait for the channels to be updated by the block commit task.
            // TODO: add a blocking method on ChainTipChange
            std::thread::sleep(Duration::from_secs(1));

            prop_assert_eq!(latest_chain_tip.best_tip_height(), Some(expected_block.height));
            prop_assert_eq!(chain_tip_change.last_tip_change(), Some(expected_action));
        }

        for block in non_finalized_blocks {
            let expected_block = block.clone();

            // The genesis block (height 0) is always finalized, and the BeforeOverwinter
            // reset fires at height 0 (one block before its activation height of 1), so
            // every non-finalized block (height >= 1) grows the chain.
            let expected_action = TipAction::grow_with(expected_block.clone().into());

            let result_receiver = state_service.queue_and_commit_to_non_finalized_state(block);
            let result = result_receiver.blocking_recv();

            prop_assert!(result.is_ok(), "unexpected failed non-finalized block commit: {:?}", result);

            // Wait for the channels to be updated by the block commit task.
            // TODO: add a blocking method on ChainTipChange
            std::thread::sleep(Duration::from_secs(1));

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
            let finalized_blocks: Vec<_> =
                blocks.into_iter().map(CheckpointVerifiedBlock).collect();

            (
                network,
                finalized_blocks.into(),
                non_finalized_blocks.into(),
            )
        })
}
