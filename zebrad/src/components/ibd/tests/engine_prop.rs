//! Property and randomized tests for the known-hash IBD engine's fetch
//! pipeline and weighted batched fetch service (design doc §4.1–4.2).
//!
//! The mock peer set serves real mainnet test-vector blocks, because the
//! batcher matches responses to requests by recomputed header hash: a fake
//! hash has no servable block.

use std::{collections::HashSet, sync::Arc, time::Duration};

use futures::future::join_all;
use proptest::prelude::*;
use rand::{rngs::StdRng, seq::SliceRandom, Rng, SeedableRng};
use tower::{Service, ServiceExt};
use tower_batch_control::{Batch, RequestWeight};

use zebra_chain::block::{self, Block};
use zebra_network::{InventoryResponse, PeerSetStatus, PeerSocketAddr};
use zebra_test::mock_service::{MockService, PanicAssertion};

use zebra_network as zn;
use zebra_state as zs;

use crate::{
    components::ibd::{
        engine::{
            ibd_max_concurrent_batches, Engine, Slot, IBD_BATCH_MAX_BLOCKS, IBD_BATCH_MAX_WEIGHT,
            SIZE_HINT_UNIT,
        },
        fetch::{BatchFetchResponse, FetchBatcher, FetchError, FetchFailureKind, FetchRequest},
        tests::FakeHashList,
    },
    BoxError,
};

use InventoryResponse::*;

/// A generous request-observation timeout for the mock services.
///
/// All these tests run under a paused Tokio clock, so waiting "longer" is
/// free; the timeout only has to stay above every engine timer the test
/// exercises (backoffs, hedges), so auto-advance reaches the engine's timer
/// first instead of failing the expectation.
const MOCK_DELAY: Duration = Duration::from_secs(120);

/// A mocked peer set service.
type MockPeerSet = MockService<zn::Request, zn::Response, PanicAssertion>;

/// A mocked state service (untouched in D2: no commit stage yet).
type MockState = MockService<zs::Request, zs::Response, PanicAssertion>;

/// A test engine over mocked services and an in-memory hash list.
type TestEngine = Engine<MockPeerSet, MockState, FakeHashList>;

/// Returns a test engine and the peer set mock handle that serves it.
///
/// `ready_peers` sizes the batch concurrency exactly like production
/// ([`ibd_max_concurrent_batches`]).
fn new_engine(
    list: FakeHashList,
    ready_peers: usize,
    lookahead_bytes: usize,
    gap_hedge_after: Duration,
) -> (TestEngine, MockPeerSet) {
    let peer_set: MockPeerSet = MockService::build()
        .with_max_request_delay(MOCK_DELAY)
        .for_unit_tests();
    let state: MockState = MockService::build().for_unit_tests();

    let (_status_sender, peer_status) = tokio::sync::watch::channel(PeerSetStatus {
        ready_peers,
        ..PeerSetStatus::default()
    });

    let engine = Engine::new(
        peer_set.clone(),
        state,
        block::Height(0),
        list,
        peer_status,
        lookahead_bytes,
        gap_hedge_after,
    );

    (engine, peer_set)
}

/// Extracts the requested hashes from a `BlocksByHash` request.
fn requested_hashes(request: &zn::Request) -> HashSet<block::Hash> {
    match request {
        zn::Request::BlocksByHash(hashes) => hashes.clone(),
        other => panic!("the engine only sends BlocksByHash, got: {other:?}"),
    }
}

/// A test peer address.
fn peer(n: u8) -> PeerSocketAddr {
    PeerSocketAddr::from(([127, 0, 0, n], 8233))
}

/// The number of items with a given size hint needed to cross the batch
/// weight threshold (the size of the flushed batch).
fn items_to_cross(size_hint: u8) -> usize {
    let weight = FetchRequest {
        height: block::Height(0),
        hash: block::Hash([0; 32]),
        size_hint,
    }
    .request_weight();

    IBD_BATCH_MAX_WEIGHT.div_ceil(weight)
}

// ------------------------------------------------------------------------
// Batched fetch service (fetch.rs) tests
// ------------------------------------------------------------------------

/// A crafted crossing batch is flushed as ONE `BlocksByHash` request and
/// served in full: the worker admits the crossing item before flushing, so
/// all-but-the-last item weigh under the threshold (design doc §4.2).
#[tokio::test(start_paused = true)]
async fn crossing_batch_is_one_request_served_in_full() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    // hint 13 weighs ~102 KB: 7 items stay under 800 KB, the 8th crosses.
    const HINT: u8 = 13;
    let batch_len = items_to_cross(HINT);
    assert_eq!(batch_len, 8, "hint 13 must cross the threshold at 8 items");

    let weight = (FetchRequest {
        height: block::Height(0),
        hash: block::Hash([0; 32]),
        size_hint: HINT,
    })
    .request_weight();
    assert!(
        (batch_len - 1) * weight < IBD_BATCH_MAX_WEIGHT,
        "all-but-the-last item must sum to under the flush threshold",
    );
    assert!(batch_len * weight >= IBD_BATCH_MAX_WEIGHT);

    let (_, blocks) = FakeHashList::continuous_mainnet(batch_len);

    let mut peer_set: MockPeerSet = MockService::build()
        .with_max_request_delay(MOCK_DELAY)
        .for_unit_tests();

    // A long flush latency isolates the weight-crossing trigger: only the
    // 8th item's crossing can flush this batch.
    let batched = Batch::new(
        FetchBatcher::new(peer_set.clone()),
        IBD_BATCH_MAX_WEIGHT,
        4,
        Duration::from_secs(60),
    );

    let item_futures = join_all(blocks.iter().enumerate().map(|(height, block)| {
        let mut service = batched.clone();
        let request = FetchRequest {
            // test heights fit u32
            height: block::Height(height as u32),
            hash: block.hash(),
            size_hint: HINT,
        };
        async move { service.ready().await?.call(request).await }
    }));

    let respond = async {
        let responder = peer_set
            .expect_request_that(|request| matches!(request, zn::Request::BlocksByHash(_)))
            .await;

        let hashes = requested_hashes(responder.request());
        assert_eq!(
            hashes,
            blocks.iter().map(|block| block.hash()).collect(),
            "the crossing batch must be flushed as one full request",
        );
        assert_eq!(hashes.len(), batch_len);

        // Serve every block, in reverse order to exercise hash matching.
        responder.respond(zn::Response::Blocks(
            blocks
                .iter()
                .rev()
                .map(|block| Available((block.clone(), Some(peer(1)))))
                .collect(),
        ));

        peer_set.expect_no_requests().await;
    };

    let (results, ()) = tokio::join!(item_futures, respond);

    for (i, result) in results.into_iter().enumerate() {
        match result? {
            BatchFetchResponse::Fetched(fetched) => {
                assert_eq!(fetched.block.hash(), blocks[i].hash());
                assert_eq!(fetched.source, Some(peer(1)));
            }
            BatchFetchResponse::Flushed => panic!("items never resolve Flushed"),
        }
    }

    Ok(())
}

/// The weight floor caps batches at the 16-block wire serving limit even for
/// minimum-hint items, and a whole-request error classifies every item in
/// the batch as a transport loss.
#[tokio::test(start_paused = true)]
async fn batch_flushes_at_wire_limit_and_transport_fails_whole_batch() {
    let _init_guard = zebra_test::init();

    assert_eq!(
        items_to_cross(1),
        IBD_BATCH_MAX_BLOCKS,
        "the weight floor must make minimum-hint batches cross at the wire limit",
    );

    let mut peer_set: MockPeerSet = MockService::build()
        .with_max_request_delay(MOCK_DELAY)
        .for_unit_tests();

    let batched = Batch::new(
        FetchBatcher::new(peer_set.clone()),
        IBD_BATCH_MAX_WEIGHT,
        4,
        Duration::from_secs(1),
    );

    // One more item than the wire limit: the 17th lands in a second batch.
    let total = IBD_BATCH_MAX_BLOCKS + 1;

    let item_futures = join_all((0..total).map(|i| {
        let mut service = batched.clone();
        let request = FetchRequest {
            // test heights fit u32
            height: block::Height(i as u32),
            // distinct fake hashes: this test only observes request shapes
            // and error classification, never a served block
            hash: block::Hash([i as u8 + 1; 32]),
            size_hint: 1,
        };
        async move { service.ready().await?.call(request).await }
    }));

    let respond = async {
        let first = peer_set
            .expect_request_that(|request| matches!(request, zn::Request::BlocksByHash(_)))
            .await;
        assert_eq!(
            requested_hashes(first.request()).len(),
            IBD_BATCH_MAX_BLOCKS,
            "the first flushed batch must hold exactly the wire limit",
        );
        first.respond(Err::<zn::Response, BoxError>(
            "synthetic transport error".into(),
        ));

        // The leftover 17th item flushes on the latency timer.
        let second = peer_set
            .expect_request_that(|request| matches!(request, zn::Request::BlocksByHash(_)))
            .await;
        assert_eq!(requested_hashes(second.request()).len(), 1);
        second.respond(Err::<zn::Response, BoxError>(
            "synthetic transport error".into(),
        ));
    };

    let (results, ()) = tokio::join!(item_futures, respond);

    for result in results {
        let error = result.expect_err("every item fails with the whole batch");
        assert_eq!(
            FetchError::classify(&error),
            FetchFailureKind::Transport,
            "whole-request errors are transport losses for every item",
        );
    }
}

/// An explicit-notfound partial response triggers exactly one in-batcher
/// retry round; a repeated notfound classifies the item as `NotFound` with
/// the reporting peer attributed from the same response's available entries.
#[tokio::test(start_paused = true)]
async fn explicit_notfound_retries_once_then_classifies() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let (_, blocks) = FakeHashList::continuous_mainnet(2);
    let (found, missing) = (blocks[0].clone(), blocks[1].clone());

    let mut peer_set: MockPeerSet = MockService::build()
        .with_max_request_delay(MOCK_DELAY)
        .for_unit_tests();

    let batched = Batch::new(
        FetchBatcher::new(peer_set.clone()),
        IBD_BATCH_MAX_WEIGHT,
        4,
        Duration::from_millis(10),
    );

    let item_futures = join_all(
        [found.clone(), missing.clone()]
            .into_iter()
            .enumerate()
            .map(|(height, block)| {
                let mut service = batched.clone();
                let request = FetchRequest {
                    // test heights fit u32
                    height: block::Height(height as u32),
                    hash: block.hash(),
                    size_hint: 13,
                };
                async move { service.ready().await?.call(request).await }
            }),
    );

    let respond = async {
        let first = peer_set
            .expect_request_that(|request| matches!(request, zn::Request::BlocksByHash(_)))
            .await;
        assert_eq!(requested_hashes(first.request()).len(), 2);

        // Peer 7 serves block 0 and explicitly reports block 1 missing.
        first.respond(zn::Response::Blocks(vec![
            Available((found.clone(), Some(peer(7)))),
            Missing(missing.hash()),
        ]));

        // One immediate in-batcher retry round for the missing remainder.
        let retry = peer_set
            .expect_request_that(|request| matches!(request, zn::Request::BlocksByHash(_)))
            .await;
        assert_eq!(
            requested_hashes(retry.request()),
            HashSet::from([missing.hash()]),
            "the retry round must re-request only the missing remainder",
        );
        retry.respond(zn::Response::Blocks(vec![Missing(missing.hash())]));

        // No further rounds: the batcher classifies and the (absent) engine
        // owns further retries.
        peer_set.expect_no_requests().await;
    };

    let (results, ()) = tokio::join!(item_futures, respond);
    let mut results = results.into_iter();

    match results.next().expect("two items")? {
        BatchFetchResponse::Fetched(fetched) => assert_eq!(fetched.block.hash(), found.hash()),
        BatchFetchResponse::Flushed => panic!("items never resolve Flushed"),
    }

    let error = results
        .next()
        .expect("two items")
        .expect_err("the missing block's item fails");
    assert_eq!(
        FetchError::classify(&error),
        FetchFailureKind::NotFound {
            peer: Some(peer(7))
        },
        "a repeated explicit notfound must attribute the reporting peer",
    );

    Ok(())
}

// ------------------------------------------------------------------------
// Engine loop (engine.rs) tests
// ------------------------------------------------------------------------

/// A NotFound response excludes the reporting peer for that height only, and
/// the engine re-requests the block after its backoff.
#[tokio::test(start_paused = true)]
async fn notfound_excludes_reporting_peer_and_rerequests() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    // hint 13 lets both heights share one batch (flushed by the 10 ms
    // latency timer), so the notfound response can attribute peer 7 from the
    // same response's available entry.
    let (list, blocks) = FakeHashList::continuous_mainnet(2);
    let list = list.with_uniform_hint(13);

    let (mut engine, mut peer_set) =
        new_engine(list, 1, 256 * 1024 * 1024, Duration::from_secs(3600));

    let respond = async {
        let first = peer_set
            .expect_request_that(|request| matches!(request, zn::Request::BlocksByHash(_)))
            .await;
        assert_eq!(
            requested_hashes(first.request()),
            blocks.iter().map(|block| block.hash()).collect(),
            "contiguous issuance must form one two-block batch",
        );
        first.respond(zn::Response::Blocks(vec![
            Available((blocks[0].clone(), Some(peer(7)))),
            Missing(blocks[1].hash()),
        ]));

        // The in-batcher retry round also comes up missing.
        let retry = peer_set
            .expect_request_that(|request| matches!(request, zn::Request::BlocksByHash(_)))
            .await;
        assert_eq!(
            requested_hashes(retry.request()),
            HashSet::from([blocks[1].hash()])
        );
        retry.respond(zn::Response::Blocks(vec![Missing(blocks[1].hash())]));

        // The engine classifies NotFound, backs off, and reassigns; the
        // paused clock auto-advances through the 500 ms backoff.
        let reissue = peer_set
            .expect_request_that(|request| matches!(request, zn::Request::BlocksByHash(_)))
            .await;
        assert_eq!(
            requested_hashes(reissue.request()),
            HashSet::from([blocks[1].hash()]),
            "the engine must re-request a notfound height",
        );
        reissue.respond(zn::Response::Blocks(vec![Available((
            blocks[1].clone(),
            Some(peer(8)),
        ))]));
    };

    let (engine_result, ()) = tokio::join!(engine.run_fetch_only(block::Height(1)), respond);
    engine_result?;

    assert_eq!(engine.fetched_blocks(), 2);
    assert_eq!(
        engine.peer_stats().excluded_peers(block::Height(1)),
        &[peer(7)],
        "the peer that reported notfound must be excluded for that height",
    );
    assert!(
        engine
            .peer_stats()
            .excluded_peers(block::Height(0))
            .is_empty(),
        "the served height must not exclude anyone",
    );
    assert_eq!(engine.peer_stats().not_found_count(peer(7)), 1);
    assert!(matches!(
        engine.slot(block::Height(1)),
        Some(Slot::Fetched { .. })
    ));

    Ok(())
}

/// A transport error re-requests the block after its backoff, without
/// excluding any peer (a timeout says nothing about peer inventory).
#[tokio::test(start_paused = true)]
async fn transport_error_rerequests_without_exclusion() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let (list, blocks) = FakeHashList::continuous_mainnet(1);
    let (mut engine, mut peer_set) =
        new_engine(list, 1, 256 * 1024 * 1024, Duration::from_secs(3600));

    let respond = async {
        let first = peer_set
            .expect_request_that(|request| matches!(request, zn::Request::BlocksByHash(_)))
            .await;
        first.respond(Err::<zn::Response, BoxError>("synthetic timeout".into()));

        // No in-batcher retry round for transport errors: the next request
        // is the engine's backed-off reassignment.
        let reissue = peer_set
            .expect_request_that(|request| matches!(request, zn::Request::BlocksByHash(_)))
            .await;
        assert_eq!(
            requested_hashes(reissue.request()),
            HashSet::from([blocks[0].hash()])
        );
        reissue.respond(zn::Response::Blocks(vec![Available((
            blocks[0].clone(),
            None,
        ))]));
    };

    let (engine_result, ()) = tokio::join!(engine.run_fetch_only(block::Height(0)), respond);
    engine_result?;

    assert_eq!(engine.fetched_blocks(), 1);
    assert_eq!(
        engine.peer_stats().excluded_len(),
        0,
        "transport errors must not exclude peers",
    );

    Ok(())
}

/// A frontier-critical height still in flight after the hedge deadline gets
/// a second, single-hash request, and the first completion wins.
#[tokio::test(start_paused = true)]
async fn gap_hedge_fires_after_deadline() -> Result<(), BoxError> {
    let _init_guard = zebra_test::init();

    let gap_hedge_after = Duration::from_secs(5);

    let (list, blocks) = FakeHashList::continuous_mainnet(1);
    let (mut engine, mut peer_set) = new_engine(list, 1, 256 * 1024 * 1024, gap_hedge_after);

    let respond = async {
        // Capture the primary fetch and stall it.
        let primary = peer_set
            .expect_request_that(|request| matches!(request, zn::Request::BlocksByHash(_)))
            .await;
        assert_eq!(
            requested_hashes(primary.request()),
            HashSet::from([blocks[0].hash()])
        );

        // While we wait here, the paused clock auto-advances to the hedge
        // deadline and the engine issues a single-hash hedge.
        let hedge = peer_set
            .expect_request_that(|request| matches!(request, zn::Request::BlocksByHash(_)))
            .await;
        let hedge_hashes = requested_hashes(hedge.request());
        assert_eq!(
            hedge_hashes,
            HashSet::from([blocks[0].hash()]),
            "the hedge must be a single-hash request for the stalled height",
        );

        hedge.respond(zn::Response::Blocks(vec![Available((
            blocks[0].clone(),
            Some(peer(2)),
        ))]));

        // The stalled primary is cancelled by the winning hedge; dropping
        // its responder is what a dead peer looks like.
        drop(primary);
    };

    let (engine_result, ()) = tokio::join!(engine.run_fetch_only(block::Height(0)), respond);
    engine_result?;

    assert_eq!(engine.hedge_count(), 1, "exactly one hedge must be issued");
    assert_eq!(engine.fetched_blocks(), 1);

    Ok(())
}

// ------------------------------------------------------------------------
// Randomized whole-pipeline properties
// ------------------------------------------------------------------------

/// The scenario invariants checked from the mock peer set side while the
/// engine runs.
struct FetchScenario {
    /// The expected blocks, indexed by hash.
    blocks_by_hash: std::collections::HashMap<block::Hash, Arc<Block>>,
    /// The size-hint upper bound for each hash, in bytes.
    hint_bytes_by_hash: std::collections::HashMap<block::Hash, u64>,
    /// The lookahead byte budget the engine was configured with.
    budget: u64,
    /// Hashes served and acknowledged.
    served: HashSet<block::Hash>,
    /// Hashes currently requested but not yet responded to.
    outstanding: HashSet<block::Hash>,
}

impl FetchScenario {
    /// Checks the per-request invariants and registers `hashes` as
    /// outstanding.
    fn on_request(&mut self, hashes: &HashSet<block::Hash>) {
        assert!(
            hashes.len() <= IBD_BATCH_MAX_BLOCKS,
            "no request may exceed the wire serving limit",
        );

        for hash in hashes {
            assert!(
                self.blocks_by_hash.contains_key(hash),
                "the engine must only request listed hashes",
            );
            assert!(
                !self.served.contains(hash),
                "issue-once: a fetched hash must never be re-requested",
            );
            assert!(
                !self.outstanding.contains(hash),
                "issue-once: an in-flight hash must never be re-requested",
            );
            self.outstanding.insert(*hash);
        }

        let outstanding_hint_bytes: u64 = self
            .outstanding
            .iter()
            .map(|hash| self.hint_bytes_by_hash[hash])
            .sum();
        assert!(
            outstanding_hint_bytes <= self.budget,
            "in-flight hint reservations must stay within the lookahead budget \
             ({outstanding_hint_bytes} > {})",
            self.budget,
        );
    }

    /// Marks `hashes` as served.
    fn on_served(&mut self, hashes: &HashSet<block::Hash>) {
        for hash in hashes {
            self.outstanding.remove(hash);
            self.served.insert(*hash);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]

    /// Under random size hints, a random byte budget, and responses
    /// completing in random order, the engine fetches every height exactly
    /// once, never exceeds the byte or wire-limit bounds while doing so, and
    /// reconciles the budget to the exact fetched sizes.
    #[test]
    fn fetch_pipeline_invariants_under_random_completion_order(
        len in 2usize..=11,
        hints in proptest::collection::vec(1u8..=255u8, 11),
        // every budget must admit at least one maximum-hint block, or the
        // engine could never issue its lowest missing height
        budget in (255 * SIZE_HINT_UNIT)..(3 * 255 * SIZE_HINT_UNIT),
        seed in any::<u64>(),
    ) {
        let (runtime, _init_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            tokio::time::pause();

            let mut rng = StdRng::seed_from_u64(seed);

            let (list, blocks) = FakeHashList::continuous_mainnet(len);
            let list = list.with_hints(&hints[..len]);

            let mut scenario = FetchScenario {
                blocks_by_hash: blocks
                    .iter()
                    .map(|block| (block.hash(), block.clone()))
                    .collect(),
                hint_bytes_by_hash: blocks
                    .iter()
                    .zip(&hints)
                    .map(|(block, hint)| (block.hash(), u64::from(*hint) * SIZE_HINT_UNIT))
                    .collect(),
                // budgets under usize::MAX by construction
                budget,
                served: HashSet::new(),
                outstanding: HashSet::new(),
            };

            // Hedging is disabled (the responder deliberately sits on
            // requests while the paused clock runs, which must not look like
            // a frontier gap), and enough ready peers for real concurrency.
            let (mut engine, mut peer_set) = new_engine(
                list,
                8,
                usize::try_from(budget).expect("test budgets fit usize"),
                Duration::from_secs(1_000_000),
            );
            assert!(ibd_max_concurrent_batches(8) > 1);

            // test lists are tiny, len - 1 fits u32
            let end = block::Height(len as u32 - 1);
            let total: usize = len;

            let respond = async {
                let mut pending: Vec<(HashSet<block::Hash>, _)> = Vec::new();

                while scenario.served.len() < total {
                    // Drain every request the engine has issued so far.
                    if let Some(responder) = peer_set.try_next_request().await {
                        let hashes = requested_hashes(responder.request());
                        scenario.on_request(&hashes);
                        pending.push((hashes, responder));
                        continue;
                    }

                    // Nothing new in flight: complete one pending request,
                    // chosen at random.
                    if pending.is_empty() {
                        continue;
                    }
                    let pick = rng.gen_range(0..pending.len());
                    let (hashes, responder) = pending.swap_remove(pick);

                    // Serve the blocks in random entry order.
                    let mut entries: Vec<_> = hashes
                        .iter()
                        .map(|hash| {
                            Available((scenario.blocks_by_hash[hash].clone(), Some(peer(1))))
                        })
                        .collect();
                    entries.shuffle(&mut rng);
                    responder.respond(zn::Response::Blocks(entries));

                    scenario.on_served(&hashes);
                }
            };

            let (engine_result, ()) = tokio::join!(engine.run_fetch_only(end), respond);
            engine_result.expect("the engine fetches the whole range");

            // test lists are tiny, len fits u64
            assert_eq!(engine.fetched_blocks(), len as u64);
            assert_eq!(engine.window_len(), len);
            assert_eq!(engine.hedge_count(), 0, "no hedges with hedging disabled");

            // Exact-byte reconciliation: after every arrival, the budget
            // holds exact serialized sizes only.
            let exact_total: u64 = blocks
                .iter()
                .map(|block| {
                    use zebra_chain::serialization::ZcashSerialize;
                    // serialized blocks are far below u64::MAX bytes
                    block.zcash_serialized_size() as u64
                })
                .sum();
            assert_eq!(engine.budget_used(), exact_total);

            for height in 0..len {
                // test lists are tiny, heights fit u32
                assert!(
                    matches!(
                        engine.slot(block::Height(height as u32)),
                        Some(Slot::Fetched { .. })
                    ),
                    "every height in the range must end Fetched",
                );
            }
        });
    }
}
